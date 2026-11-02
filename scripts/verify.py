#!/usr/bin/env python3
"""End-to-end verification of the release build.

Runs the real binary on the fixtures (live dashboard in a pseudo-terminal,
--plain, --json), checks outputs, cancellation, resizing, colour, bad inputs,
the model proposer against mock and unreachable endpoints, and repeats the
confirmed speedups. Writes release-run/verify/results.json and artefacts.

Usage: python3 scripts/verify.py [core] [stress] [model]
       (after `cargo build --release`; no argument runs core and stress)
       `model` runs the search against a real local model; set ACO_VERIFY_ENDPOINT
       (an OpenAI-compatible server, e.g. llama.cpp) and ACO_VERIFY_MODEL.
Needs: python3, pyte (pip install pyte) for the terminal checks, network access
for the package-install rows.
"""
import fcntl
import http.server
import json
import os
import pty
import re
import select
import signal
import statistics
import struct
import subprocess
import sys
import termios
import threading
import time
from pathlib import Path

import pyte

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target/release/aco"
OUT = ROOT / "release-run/verify"
FIX = ["bubble", "concat", "lookup", "slower"]
results = []


def record(row, name, ok, detail):
    results.append({"row": row, "name": name, "pass": bool(ok), "detail": detail})
    print(f"[{'PASS' if ok else 'FAIL'}] {row} · {name}: {detail}", flush=True)


def leftovers():
    """Sandbox processes still alive (should be none between runs)."""
    r = subprocess.run(["pgrep", "-af", "aco-sandbox|/work/harness.py"], capture_output=True, text=True)
    return [l for l in r.stdout.splitlines() if "pgrep" not in l]


# ------------------------------------------------------------------ pty

class Pty:
    def __init__(self, argv, cols=120, rows=36, env=None, cwd=ROOT):
        self.cols, self.rows = cols, rows
        self.master, self.slave = os.openpty()
        self.set_size(cols, rows)
        self.attrs_before = termios.tcgetattr(self.slave)
        e = dict(os.environ)
        e.update({"TERM": "xterm-256color", "COLUMNS": str(cols), "LINES": str(rows)})
        e.pop("NO_COLOR", None)
        if env:
            e.update(env)

        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.p = subprocess.Popen(
            argv, stdin=self.slave, stdout=self.slave, stderr=self.slave,
            cwd=cwd, env=e, preexec_fn=child_setup, close_fds=True,
        )
        self.raw = b""
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.saw = set()

    def set_size(self, cols, rows):
        self.cols, self.rows = cols, rows
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def resize(self, cols, rows):
        self.set_size(cols, rows)
        self.screen.resize(rows, cols)
        # The kernel signals the foreground process group on a size change.

    def pump(self, t=0.05):
        r, _, _ = select.select([self.master], [], [], t)
        if r:
            try:
                data = os.read(self.master, 65536)
            except OSError:
                return
            self.raw += data
            self.stream.feed(data)

    def text(self):
        return "\n".join(self.screen.display)

    def wait_for(self, pattern, timeout):
        end = time.time() + timeout
        rx = re.compile(pattern)
        while time.time() < end:
            self.pump()
            if rx.search(self.text()):
                return True
            if self.p.poll() is not None:
                self.pump(0.2)
                return bool(rx.search(self.text()))
        return False

    def send(self, data):
        os.write(self.master, data)

    def finish(self, timeout):
        end = time.time() + timeout
        while time.time() < end and self.p.poll() is None:
            self.pump()
        for _ in range(10):
            self.pump(0.02)
        code = self.p.poll()
        if code is None:
            self.p.kill()
            self.p.wait()
        attrs_after = termios.tcgetattr(self.slave)
        os.close(self.slave)
        os.close(self.master)
        return code, attrs_after == self.attrs_before

    def restored_sequences(self):
        raw = self.raw
        entered = b"\x1b[?1049h" in raw
        left = raw.rfind(b"\x1b[?1049l") > raw.rfind(b"\x1b[?1049h")
        cursor = raw.rfind(b"\x1b[?25h") > raw.rfind(b"\x1b[?25l")
        return entered and left and cursor


def run(args, **kw):
    t = time.time()
    p = subprocess.run([str(BIN), *args], capture_output=True, text=True, cwd=ROOT, **kw)
    return p, time.time() - t


# --------------------------------------------------------------- checks

def check_report(path, reps):
    """Schema and internal-consistency checks for one report."""
    v = json.loads(Path(path).read_text())
    problems = []
    need = {"schema_version": int, "tool": str, "file": str, "sandbox": str, "proposer": str,
            "config": dict, "baseline": dict, "candidates": list, "generations": list,
            "stop_reason": str, "best": dict, "best_source": str}
    for k, t in need.items():
        if not isinstance(v.get(k), t):
            problems.append(f"{k} missing or not {t.__name__}")
    if "confirmation" not in v:
        problems.append("confirmation key missing")
    b = v.get("baseline", {})
    if len(b.get("times_ns", [])) != reps:
        problems.append(f"baseline has {len(b.get('times_ns', []))} times, want {reps}")
    elif b["median_ns"] != int(statistics.median(b["times_ns"])):
        problems.append("baseline median is not the median of its times")
    for c in v.get("candidates", []):
        if c["decision"] not in {"keep", "reject_policy", "reject_timeout", "reject_error",
                                 "reject_oracle", "reject_margin"}:
            problems.append(f"bad decision {c['decision']}")
        if c["median_ns"] is not None and c["median_ns"] != int(statistics.median(c["times_ns"])):
            problems.append(f"candidate {c['index']} median mismatch")
    conf = v.get("confirmation")
    best = v.get("best", {})
    if conf:
        bm, cm = conf["baseline_median_ns"], conf["candidate_median_ns"]
        if len(conf["baseline_times_ns"]) != conf["rounds"] and conf["passed"]:
            problems.append("confirmation rounds incomplete but passed")
        if abs(conf["speedup"] - bm / cm) > 1e-9:
            problems.append("confirmation speedup != baseline/candidate")
        if best.get("confirmed") != conf["passed"]:
            problems.append("best.confirmed disagrees with confirmation.passed")
    if best.get("confirmed"):
        if best["speedup"] < 1 + v["config"]["margin"]:
            problems.append("confirmed below margin")
    else:
        src = (ROOT / v["file"]).read_text() if (ROOT / v["file"]).exists() else None
        if src is not None and v["best_source"] != src:
            problems.append("unconfirmed best_source differs from the original")
    return v, problems


def same_stdout(orig, best):
    a = subprocess.run([sys.executable, orig], capture_output=True, text=True, timeout=600)
    b = subprocess.run([sys.executable, best], capture_output=True, text=True, timeout=600)
    return a.returncode == 0 and b.returncode == 0 and a.stdout == b.stdout, a.stdout.strip()[:60]


def tui_fixture(name, out):
    t = Pty([str(BIN), "--json", "--out", str(out), f"fixtures/{name}.py"])
    done = t.wait_for(r"q quit", 300)
    screen = t.text()
    (OUT / f"tui-{name}.screen.txt").write_text(screen + "\n")
    t.send(b"q")
    code, attrs_ok = t.finish(10)
    after = t.raw.decode("utf-8", "replace")
    ok = done and code == 0 and attrs_ok and t.restored_sequences() and \
        ("faster, confirmed" in after or "original kept" in after) and \
        (out / f"{name}.best.py").exists() and (out / f"{name}.report.json").exists()
    m = re.search(r"([\d.]+)x faster, confirmed", screen)
    detail = (f"exit {code}; summary panel: {m.group(0) if m else 'original kept' if 'original kept' in screen else '?'}; "
              f"terminal restored: {attrs_ok and t.restored_sequences()}")
    record("TUI", f"fixtures/{name}.py", ok, detail)


def main():
    if not BIN.exists():
        sys.exit("build first: cargo build --release")
    OUT.mkdir(parents=True, exist_ok=True)
    version = subprocess.run([str(BIN), "--version"], capture_output=True, text=True).stdout.strip()
    py = subprocess.run(["python3", "--version"], capture_output=True, text=True).stdout.strip()
    print(version, py, flush=True)
    want = set(sys.argv[1:]) or {"core", "stress"}
    if "core" in want:
        core()
    if "stress" in want:
        stress()
    if "model" in want:
        real_model()

    name = "results.json" if want >= {"core", "stress"} else f"results-{'-'.join(sorted(want))}.json"
    (OUT / name).write_text(json.dumps({"version": version, "python": py, "results": results}, indent=2) + "\n")
    failed = [r for r in results if not r["pass"]]
    print(f"\n{len(results) - len(failed)}/{len(results)} passed", flush=True)
    sys.exit(1 if failed else 0)


def core():
    # 1. Every fixture through the live dashboard (with --json).
    for name in FIX:
        tui_fixture(name, OUT / "tui")

    # 2. Every fixture through --plain --json, three times (repeatability).
    speed = {n: [] for n in FIX}
    for rep in (1, 2, 3):
        out = OUT / f"plain-{rep}"
        for name in FIX:
            p, secs = run(["--plain", "--json", "--out", str(out), f"fixtures/{name}.py"])
            (OUT / f"plain-{rep}-{name}.txt").write_text(p.stdout + p.stderr)
            v, problems = check_report(out / f"{name}.report.json", 5)
            speed[name].append(v["best"]["speedup"] if v["best"]["confirmed"] else None)
            if rep == 1:
                ok = p.returncode == 0 and "\x1b" not in p.stdout and not problems
                b = v["best"]
                record("--plain", f"fixtures/{name}.py", ok,
                       f"exit {p.returncode}, {secs:.0f}s; " +
                       (f"confirmed {b['speedup']:.1f}x ({b['origin']})" if b["confirmed"] else "original kept") +
                       ("; no ANSI codes in piped output" if "\x1b" not in p.stdout else "; ANSI in piped output"))
                record("--json", f"{name}.report.json", not problems,
                       "schema_version 1, keys/types, medians recomputed, speedup = baseline/candidate"
                       + (f"; problems: {problems}" if problems else ""))
                eq, sample = same_stdout(ROOT / f"fixtures/{name}.py", out / f"{name}.best.py")
                record("best.py == original stdout", f"{name}", eq, f"stdout identical: {sample!r}")
                if name == "slower":
                    rej = [c for c in v["candidates"]]
                    ok = (not b["confirmed"]) and all(c["decision"] == "reject_margin" for c in rej) and rej and \
                        (out / "slower.best.py").read_text() == (ROOT / "fixtures/slower.py").read_text()
                    record("slower.py rejected", "slower.py", ok,
                           "; ".join(f"{c['origin']} {c['median_ns']/1e6:.1f} ms ({c['speedup']:.2f}x) {c['decision']}"
                                     for c in rej) + f" vs baseline {v['baseline']['median_ns']/1e6:.1f} ms; best.py is the original")
    for name in ["bubble", "concat", "lookup"]:
        xs = speed[name]
        ok = all(x is not None for x in xs)
        if ok:
            spread = (max(xs) - min(xs)) / statistics.mean(xs) * 100
            record("repeatability", name, ok,
                   f"confirmed 3/3: {', '.join(f'{x:.1f}x' for x in xs)} (spread {spread:.0f}% of mean)")
        else:
            record("repeatability", name, False, f"{xs}")
    record("repeatability", "slower", all(x is None for x in speed["slower"]),
           "rejected 3/3" if all(x is None for x in speed["slower"]) else str(speed["slower"]))

    # 3. Search controls on real runs.
    for args, want_gens, want_eval, label in [
        (["--gens", "1"], 1, None, "--gens 1 stops after one generation"),
        (["--pop", "2"], None, 1, "--pop 2 measures 1 candidate per generation"),
        (["--pop", "4"], None, 2, "--pop 4 measures both rule rewrites (rules offer 2)"),
    ]:
        out = OUT / "controls"
        p, _ = run(["--plain", "--json", "--reps", "3", "--out", str(out), *args, "fixtures/bubble.py"])
        v = json.loads((out / "bubble.report.json").read_text())
        g = v["generations"]
        ok = p.returncode == 0
        if want_gens is not None:
            ok &= len(g) == want_gens and v["stop_reason"] == f"reached --gens {want_gens}"
        if want_eval is not None:
            ok &= g[0]["evaluated"] == want_eval
        record("search controls", label, ok,
               f"generations {len(g)}, evaluated per gen {[x['evaluated'] for x in g]}, stop: {v['stop_reason']}")
    out = OUT / "controls"
    p, _ = run(["--plain", "--json", "--reps", "3", "--patience", "3", "--gens", "5", "--out", str(out), "fixtures/slower.py"])
    v = json.loads((out / "slower.report.json").read_text())
    record("search controls", "--patience 3 on slower.py (rules exhaust first)", p.returncode == 0,
           f"generations {len(v['generations'])}, stop: {v['stop_reason']}; patience itself is covered by "
           "tests/search_fake.rs (no_improvement_stops_after_patience)")

    # 4. Bad inputs, policy, sandbox limits through the CLI.
    bad = OUT / "inputs"
    bad.mkdir(exist_ok=True)
    cases = [
        ("missing file", ["nope.py"], 2, "does not exist"),
        ("syntax error", [str(bad / "syntax.py")], 2, "syntax.py:2:"),
        ("empty file", [str(bad / "empty.py")], 2, "has no code to optimise"),
        ("comments only", [str(bad / "comments.py")], 2, "has no code to optimise"),
        ("non-Python text", ["README.md"], 2, "is not valid Python"),
        ("binary file", [str(bad / "blob.py")], 2, "contains NUL bytes"),
        ("directory", ["fixtures"], 2, "is a directory"),
        ("policy: import os", [str(bad / "p_os.py")], 2, "banned module `os`"),
        ("policy: subprocess", [str(bad / "p_sub.py")], 2, "banned module `subprocess`"),
        ("policy: open()", [str(bad / "p_open.py")], 2, "banned builtin `open`"),
        ("policy: eval()", [str(bad / "p_eval.py")], 2, "banned builtin `eval`"),
        ("policy: socket", [str(bad / "p_sock.py")], 2, "banned module `socket`"),
        ("policy: __import__", [str(bad / "p_imp.py")], 2, "`__import__`"),
        ("policy: dunder escape", [str(bad / "p_dunder.py")], 2, "dunder attribute"),
        ("sandbox timeout", ["--plain", "--timeout", "2", str(bad / "spin.py")], 1, "timed out"),
        ("sandbox memory limit", ["--plain", "--memory-mb", "128", str(bad / "hog.py")], 1, "MemoryError"),
        ("crashing script", ["--plain", str(bad / "crash.py")], 1, "ZeroDivisionError"),
        ("no sandbox (docker absent)", ["--sandbox", "docker", "fixtures/lookup.py"], 3, "no sandbox is available"),
        ("usage: --reps 4", ["--reps", "4", "x.py"], 2, "must be odd"),
        ("usage: --pop 1", ["--pop", "1", "x.py"], 2, "at least 2"),
    ]
    files = {
        "syntax.py": "x = 1\ndef f(:\n    pass\n", "empty.py": "", "comments.py": "# nothing here\n",
        "p_os.py": "import os\nprint(os.listdir('/'))\n",
        "p_sub.py": "import subprocess\nsubprocess.run(['id'])\n",
        "p_open.py": "print(open('/etc/passwd').read())\n",
        "p_eval.py": "print(eval('2+2'))\n",
        "p_sock.py": "import socket\nsocket.create_connection(('1.1.1.1', 53))\n",
        "p_imp.py": "os = __import__('os')\n",
        "p_dunder.py": "print(().__class__.__base__.__subclasses__())\n",
        "spin.py": "while True:\n    pass\n",
        "hog.py": "b = bytearray(512 * 1024 * 1024)\nprint(len(b))\n",
        "crash.py": "print(1 / 0)\n",
    }
    for n, body in files.items():
        (bad / n).write_text(body)
    (bad / "blob.py").write_bytes(bytes([0x7F, 0x45, 0x4C, 0x46, 0xFF, 0xFE, 0, 1]))
    docker_present = subprocess.run(["sh", "-c", "command -v docker"], capture_output=True).returncode == 0
    for label, args, code, needle in cases:
        if label.startswith("no sandbox") and docker_present:
            record("bad input", label, True, "skipped: docker is installed here")
            continue
        p, _ = run(args, timeout=120)
        msg = (p.stderr + p.stdout).strip()
        ok = p.returncode == code and needle in msg
        first = next((l for l in msg.splitlines() if l.startswith("error")), msg.splitlines()[0] if msg else "")
        record("bad input" if not label.startswith(("policy", "sandbox")) else label.split(":")[0].split(" ")[0],
               label, ok, f"exit {p.returncode}: {first[:150]}")

    # 5. Model proposer against a mock server and unreachable endpoints.
    lookup = (ROOT / "fixtures/lookup.py").read_text()
    replies = {
        "good": "```python\n" + lookup.replace("hits = 0\n", "hits = 0\nallowed_s = set(allowed)\n", 1)
                .replace("if k in allowed:", "if k in allowed_s:") + "```",
        "policy": "```python\nimport os\nprint(os.getcwd())\n```",
        "wrong": "```python\nprint(0)\n```",
    }

    class H(http.server.BaseHTTPRequestHandler):
        mode = "good"

        def do_POST(self):
            self.rfile.read(int(self.headers.get("content-length", 0)))
            if H.mode == "garbage":
                body = b"not json"
            elif H.mode == "500":
                body = b'{"error": "model not loaded"}'
            else:
                body = json.dumps({"choices": [{"message": {"content": replies[H.mode]}}],
                                   "usage": {"completion_tokens": 42}}).encode()
            self.send_response(200 if H.mode != "500" else 500)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *a):
            pass

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    ep = f"http://127.0.0.1:{srv.server_address[1]}"
    for mode, expect in [("good", "confirmed"), ("policy", "dropped by policy"),
                         ("wrong", "output differs"), ("garbage", "failed"), ("500", "failed")]:
        H.mode = mode
        out = OUT / f"model-{mode}"
        p, secs = run(["--plain", "--json", "--reps", "3", "--proposer", "ollama", "--endpoint", ep,
                       "--out", str(out), "fixtures/lookup.py"], timeout=300)
        text = p.stdout + p.stderr
        v = json.loads((out / "lookup.report.json").read_text())
        if mode == "good":
            ok = p.returncode == 0 and v["best"]["confirmed"] and v["best"]["origin"].startswith("model:")
            d = f"mock reply kept and confirmed at {v['best']['speedup']:.1f}x, tokens counted {v['candidates'][0]['tokens']}"
        elif mode == "wrong":
            ok = p.returncode == 0 and any(c["decision"] == "reject_oracle" for c in v["candidates"]) and not v["best"]["confirmed"]
            d = "a reply that prints something else is measured and rejected: output differs"
        else:
            ok = p.returncode == 0 and expect in text and not v["best"]["confirmed"] and not v["candidates"]
            note = next((l.strip() for l in text.splitlines() if "note:" in l), "")
            d = f"exit 0, original kept; {note[:140]}"
        record("model proposer", f"mock server: {mode}", ok, d)
    srv.shutdown()
    for label, ep2, limit in [("refused (nothing listening)", "http://127.0.0.1:9", 30),
                              ("unroutable address", "http://10.255.255.1:11434", 60)]:
        p, secs = run(["--plain", "--reps", "3", "--proposer", "ollama", "--endpoint", ep2,
                       "--out", str(OUT / "model-x"), "fixtures/lookup.py"], timeout=300)
        text = p.stdout + p.stderr
        note = next((l.strip() for l in text.splitlines() if "note:" in l), "")
        ok = p.returncode == 0 and "failed" in note and secs < limit
        record("model proposer", label, ok, f"exit {p.returncode} in {secs:.1f}s; {note[:150]}")

    # 6. Cancel, signals, panic, resize, colour.
    def cancel_case(label, argv, action, want_code, env=None, wait=r"candidate \d+/\d+|confirming"):
        t = Pty(argv, env=env)
        started = t.wait_for(wait, 120) if wait else True
        t0 = time.time()
        action(t)
        code, attrs_ok = t.finish(20)
        secs = time.time() - t0
        time.sleep(0.3)
        left = leftovers()
        seq = t.restored_sequences() if "--plain" not in argv else True
        ok = started and code == want_code and attrs_ok and seq and not left
        record("cancel", label, ok,
               f"exit {code} {secs:.1f}s after the action; termios restored: {attrs_ok}; "
               f"alt-screen left + cursor shown: {seq}; leftover sandbox processes: {len(left)}")
        return t

    tui = [str(BIN), "--out", str(OUT / "cancel"), "fixtures/bubble.py"]
    cancel_case("Ctrl-C in the dashboard mid-search", tui, lambda t: t.send(b"\x03"), 130)
    cancel_case("q in the dashboard mid-search", tui, lambda t: t.send(b"q"), 130)
    cancel_case("Ctrl-C during confirmation", tui, lambda t: t.send(b"\x03"), 130, wait=r"confirming · round [1-9]")
    cancel_case("SIGTERM to the dashboard", tui, lambda t: t.p.send_signal(signal.SIGTERM), 130)
    cancel_case("Ctrl-C in --plain (SIGINT from the tty)", [str(BIN), "--plain", "--out", str(OUT / "cancel"), "fixtures/bubble.py"],
                lambda t: t.send(b"\x03"), 130, wait=r"baseline")
    cancel_case("panic in the search thread (ACO_TEST_PANIC=1)", tui, lambda t: None, 101,
                env={"ACO_TEST_PANIC": "1"}, wait=None)

    # Resize: shrink below the minimum mid-run, then grow; the final screen must be whole.
    t = Pty([str(BIN), "--out", str(OUT / "resize"), "fixtures/lookup.py"], cols=120, rows=36)
    t.wait_for(r"candidate", 120)
    t.resize(100, 30)
    t.wait_for(r"candidates", 5)
    t.resize(60, 18)
    small = t.wait_for(r"terminal too small", 5)
    t.resize(140, 42)
    done = t.wait_for(r"q quit", 180)
    final = t.text()
    (OUT / "resize-final.screen.txt").write_text(final + "\n")
    t.send(b"q")
    code, attrs_ok = t.finish(10)
    widths_ok = max(len(l.rstrip()) for l in final.splitlines()) <= 140
    ok = small and done and code == 0 and attrs_ok and "summary" in final and "diff" in final and widths_ok
    record("resize", "120x36 → 100x30 → 60x18 → 140x42 mid-run", ok,
           f"'terminal too small' shown at 60x18: {small}; final 140x42 frame has summary and diff: "
           f"{'summary' in final and 'diff' in final}; exit {code}")

    # Colour: TTY, NO_COLOR on a TTY, and a pipe.
    tiny = OUT / "inputs" / "tiny.py"
    tiny.write_text("total = 0\nfor i in range(1000):\n    total += i\nprint(total)\n")
    t = Pty([str(BIN), "--plain", "--out", str(OUT / "colour"), str(tiny)])
    t.finish(60)
    tty_colour = b"\x1b[1;36maco" in t.raw
    t = Pty([str(BIN), "--plain", "--out", str(OUT / "colour"), str(tiny)], env={"NO_COLOR": "1"})
    t.finish(60)
    nocolor = b"\x1b[" not in t.raw
    p, _ = run(["--out", str(OUT / "colour"), str(tiny)])
    piped = p.returncode == 0 and "\x1b" not in p.stdout and "no faster version found" in p.stdout
    record("colour", "--plain on a TTY is coloured", tty_colour, "SGR codes present")
    record("colour", "NO_COLOR=1 on a TTY", nocolor, "no escape codes at all")
    record("colour", "stdout piped (no --plain)", piped, "falls back to line output, no escape codes")

    # Closing the terminal while the dashboard waits for q (window closed,
    # ssh dropped). At 454c873 aco spun at 100% CPU here forever.
    for label, ctty in [("controlling terminal closed after the run (SIGHUP)", True),
                        ("stdin/stdout terminal closed, no SIGHUP", False)]:
        m, sl = os.openpty()
        fcntl.ioctl(sl, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))

        def mk():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        env = dict(os.environ, TERM="xterm-256color")
        p = subprocess.Popen([str(BIN), "--out", str(OUT / "hangup"), "fixtures/lookup.py"], stdin=sl, stdout=sl,
                             stderr=subprocess.DEVNULL, cwd=ROOT, env=env,
                             preexec_fn=mk if ctty else None, start_new_session=not ctty)
        os.close(sl)
        seen, end = b"", time.time() + 180
        while time.time() < end and b"q quit" not in seen:
            r, _, _ = select.select([m], [], [], 0.2)
            if r:
                try:
                    seen = (seen + os.read(m, 65536))[-100000:]
                except OSError:
                    break
        done = b"q quit" in seen
        os.close(m)
        t0 = time.time()
        try:
            code = p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            code = None
            p.kill()
            p.wait()
        secs = time.time() - t0
        left = leftovers()
        record("hang-up", label, done and code == 130 and not left,
               (f"exited {secs:.2f}s after the terminal closed (exit {code}); leftover sandbox processes: {len(left)}"
                if code is not None else "STILL RUNNING 5s after the terminal closed"))


# ----------------------------------------------------------- stress pass

S = "fixtures/stress"


def plain(label_out, args, reps=3, env=None, timeout=900):
    """Run --plain --json; return (proc, report or None, seconds)."""
    out = OUT / "stress" / label_out
    e = dict(os.environ)
    e.setdefault("ACO_CACHE_DIR", str(OUT / "cache"))
    if env:
        e.update(env)
    t = time.time()
    p = subprocess.run([str(BIN), "--plain", "--color", "never", "--json", "--reps", str(reps), "--out", str(out), *args],
                       capture_output=True, text=True, cwd=ROOT, env=e, timeout=timeout)
    secs = time.time() - t
    (OUT / "stress" / f"{label_out}.txt").write_text(p.stdout + p.stderr)
    reports = sorted(out.glob("*.report.json")) if out.exists() else []
    newest = max(reports, key=lambda x: x.stat().st_mtime) if reports else None
    v = json.loads(newest.read_text()) if newest and newest.stat().st_mtime >= t - 1 else None
    return p, v, secs


def first_error(p):
    return next((l for l in (p.stderr + p.stdout).splitlines() if l.startswith("error")), "")[:220]


def outcome(v):
    b = v["best"]
    return (f"confirmed {b['speedup']:.1f}x ({fmt_ns(v['confirmation']['baseline_median_ns'])} → "
            f"{fmt_ns(v['confirmation']['candidate_median_ns'])})" if b["confirmed"] else "original kept")


def fmt_ns(ns):
    return f"{ns / 1e6:.1f} ms" if ns >= 1e6 else f"{ns / 1e3:.1f} µs"


def stdout_of(script, cwd=None, stdin=None, args=()):
    r = subprocess.run([sys.executable, str(script), *args], capture_output=True, cwd=cwd, timeout=600,
                       input=stdin)
    return r.returncode, r.stdout, r.stderr


def stress():
    (OUT / "stress").mkdir(parents=True, exist_ok=True)

    # Large single files: the same hot spots repeated through 300-3000 lines.
    for n in (300, 1000, 3000):
        p, v, secs = plain(f"large_{n}", [f"{S}/large_{n}.py"])
        t = v["timings"] if v else {}
        same = v and stdout_of(ROOT / f"{S}/large_{n}.py")[1] == stdout_of(OUT / f"stress/large_{n}/large_{n}.best.py")[1]
        ok = p.returncode == 0 and v and v["best"]["confirmed"] and same
        record("large file", f"large_{n}.py ({n} lines)", ok,
               f"{outcome(v)}; {len(v['candidates'])} candidates; parse {t['parse_ns']/1e6:.1f} ms, "
               f"policy {t['policy_ns']/1e6:.1f} ms, rewrite rules {t['propose_ns']/1e6:.0f} ms, "
               f"search {t['search_ns']/1e9:.1f} s; wall {secs:.1f} s; best.py stdout identical: {bool(same)}")

    # Multi-file project.
    p, v, _ = plain("project", [f"{S}/project/main.py"])
    files = [f["path"] for f in v["run"]["files"]] if v else []
    ok = p.returncode == 0 and len(files) == 6 and any("--mutate" in n and "shop/inventory.py" in n for n in v["notes"])
    record("project", "main.py imports 5 local modules (package, relative imports)", ok,
           f"{len(files)} files copied in ({', '.join(files)}); note: {v['notes'][0][:120] if v and v['notes'] else '-'}")
    p, v, _ = plain("project-mutate", ["--mutate", f"{S}/project/shop/inventory.py", f"{S}/project/main.py"])
    best = OUT / "stress/project-mutate/inventory.best.py"
    # Run the project with the rewritten module dropped in, outside aco.
    import shutil, tempfile
    with tempfile.TemporaryDirectory() as d:
        shutil.copytree(ROOT / f"{S}/project", Path(d) / "p")
        before = stdout_of(Path(d) / "p/main.py", cwd=Path(d) / "p")[1]
        shutil.copy(best, Path(d) / "p/shop/inventory.py")
        after = stdout_of(Path(d) / "p/main.py", cwd=Path(d) / "p")[1]
    untouched = subprocess.run(["git", "diff", "--quiet", "--", f"{S}/project"], cwd=ROOT).returncode == 0
    ok = p.returncode == 0 and v["best"]["confirmed"] and before == after and untouched
    record("project", "--mutate shop/inventory.py", ok,
           f"{outcome(v)}; project stdout with inventory.best.py dropped in identical: {before == after}; "
           f"source files unchanged: {untouched}")

    # A generated 400-module project: analysis cost and caps. 20 chains of 20
    # modules each (a chain of 400 nested imports overflows CPython's own stack).
    import tempfile
    big = Path(tempfile.mkdtemp(prefix="aco-big-"))
    (big / "pkg").mkdir()
    (big / "pkg/__init__.py").write_text("")
    for c in range(20):
        for k in range(20):
            i = c * 20 + k
            nxt = f"from pkg import m{i + 1}\n" if k < 19 else ""
            body = "def f(x):\n    return x + 1\n" if i != 210 else (
                "def f(x):\n    allowed = [i * 3 for i in range(3000)]\n    hits = 0\n"
                "    for k in range(20000):\n        if k in allowed:\n            hits += 1\n    return x + hits\n")
            (big / f"pkg/m{i}.py").write_text(nxt + body)
    (big / "main.py").write_text("".join(f"from pkg import m{c * 20}\n" for c in range(20))
                                 + "from pkg import m210\nprint(m0.f(1), m210.f(0))\n")
    p, v, secs = plain("big-project", ["--mutate", str(big / "pkg/m210.py"), str(big / "main.py")])
    ok = p.returncode == 0 and v and len(v["run"]["files"]) == 402 and v["best"]["confirmed"]
    record("project", "generated project: 400 modules (20 import chains of 20), hot spot in pkg/m210.py", ok,
           (f"{len(v['run']['files'])} files; import analysis {v['timings']['project_ns']/1e6:.0f} ms; "
            f"{outcome(v)}; wall {secs:.1f} s") if v else first_error(p))
    for i in range(1001):
        (big / f"extra_{i}.py").write_text("x = 1\n")
    (big / "main.py").write_text("".join(f"import extra_{i}\n" for i in range(1001)) + "print(1)\n")
    p, v, _ = plain("too-many", [str(big / "main.py")])
    record("project", "1003-module closure is refused (cap 1000 files)", p.returncode == 2 and "1000" in first_error(p),
           f"exit {p.returncode}: {first_error(p)}")
    shutil.rmtree(big)

    p, v, _ = plain("stdlib", [f"{S}/stdlib_heavy.py"])
    record("stdlib", "stdlib_heavy.py (collections, itertools, heapq, re, json, dataclasses …)",
           p.returncode == 0 and v is not None, f"exit {p.returncode}; {outcome(v)}; baseline {fmt_ns(v['baseline']['median_ns'])}")

    # Third-party packages. A fresh cache measures the install; the rerun reuses it.
    cache = OUT / "cache-fresh"
    shutil.rmtree(cache, ignore_errors=True)
    p, v, secs = plain("numpy", [f"{S}/numpy_project/stats.py"], env={"ACO_CACHE_DIR": str(cache)})
    pk = ", ".join(f"{x['name']} {x['version']}" for x in v["run"]["packages"]) if v else ""
    ok = p.returncode == 0 and v and v["best"]["confirmed"] and "numpy 2.3.4" in pk
    record("packages", "numpy_project (numpy + more-itertools), fresh cache", ok,
           f"{outcome(v)}; installed {pk}; install {v['timings']['deps_ns']/1e9:.1f} s (outside the timed runs); wall {secs:.1f} s")
    p, v, secs = plain("numpy-cached", [f"{S}/numpy_project/stats.py"], env={"ACO_CACHE_DIR": str(cache)})
    ok = p.returncode == 0 and "using cached packages" in p.stderr + p.stdout
    record("packages", "numpy_project rerun reuses the cache", ok,
           f"deps step {v['timings']['deps_ns']/1e6:.0f} ms; {outcome(v)}")
    p, _, _ = plain("missing", [f"{S}/missing_dep.py"])
    record("packages", "import of an undeclared package", p.returncode == 2 and "Add `PyYAML`" in first_error(p),
           f"exit {p.returncode}: {first_error(p)}")
    p, _, _ = plain("badreq", [f"{S}/bad_requirements/use.py"])
    record("packages", "requirements.txt with a pip option (-e git+…)", p.returncode == 2 and "not supported" in first_error(p),
           f"exit {p.returncode}: {first_error(p)}")
    d = Path(tempfile.mkdtemp(prefix="aco-nowheel-"))
    (d / "requirements.txt").write_text("pycairo==1.20.0\n")
    (d / "main.py").write_text("import cairo\nprint(1)\n")
    p, _, _ = plain("nowheel", [str(d / "main.py")])
    record("packages", "package with no wheel (pycairo 1.20.0, sdist only)",
           p.returncode == 2 and "no installable wheel" in first_error(p), f"exit {p.returncode}: {first_error(p)}")
    shutil.rmtree(d)

    # Semantics traps: rewrites that look right but change behaviour.
    for name, why in [("alias_set", "list mutated through an alias after the loop"),
                      ("global_mutation", "list mutated by a function through `global`"),
                      ("partial_bubble", "bubble loop that stops early (not a full sort)"),
                      ("loopvar_bubble", "loop variable read after the sort")]:
        p, v, _ = plain(f"trap-{name}", [f"{S}/traps/{name}.py"])
        ok = p.returncode == 0 and v and not v["candidates"] and not v["best"]["confirmed"]
        record("semantics trap", f"{name}.py: {why}", ok,
               f"no rewrite proposed (rule refuses statically); {len(v['candidates'])} candidates; original kept"
               " (at 454c873 the rule fired and only the output check caught it)")
    p, v, _ = plain("trap-duplicates", [f"{S}/traps/duplicates_order.py"])
    same = stdout_of(ROOT / f"{S}/traps/duplicates_order.py")[1] == stdout_of(OUT / "stress/trap-duplicates/duplicates_order.best.py")[1]
    record("semantics trap", "duplicates_order.py: order-preserving dedup keeps order", p.returncode == 0 and v["best"]["confirmed"] and same,
           f"{outcome(v)}; best.py stdout identical: {same}")
    p, v, _ = plain("trap-setout", [f"{S}/traps/set_output.py"])
    record("semantics trap", "set_output.py prints a set (hash-order output)", p.returncode == 0 and v["best"]["confirmed"],
           f"{outcome(v)}; baseline and candidate share each round's PYTHONHASHSEED, so a correct rewrite is accepted "
           "(at 454c873 random per-process seeds rejected it as 'output differs')")

    # hashseed: a rewrite to list(set(...)) matches at seed 0 only. Served by a mock model.
    src = (ROOT / f"{S}/traps/hashseed.py").read_text()
    bad = src.split("seen = []")[0] + "seen = list(set(words))\nprint(seen)\n"

    class H(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get("content-length", 0)))
            body = json.dumps({"choices": [{"message": {"content": "```python\n" + bad + "```"}}],
                               "usage": {"completion_tokens": 9}}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *a):
            pass
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    p, v, _ = plain("trap-hashseed", ["--proposer", "ollama", "--endpoint", f"http://127.0.0.1:{srv.server_address[1]}",
                                      f"{S}/traps/hashseed.py"])
    srv.shutdown()
    kept = [c for c in v["candidates"] if c["decision"] == "keep"]
    conf = v.get("confirmation") or {}
    ok = p.returncode == 0 and kept and not v["best"]["confirmed"] and "PYTHONHASHSEED" in (conf.get("detail") or "")
    record("semantics trap", "hashseed.py: list(set(…)) matches only under seed 0", ok,
           f"passed the search (seed 0) at {kept[0]['speedup']:.0f}x, then confirmation: {(conf.get("detail") or "?")[:110]}"
           if kept else f"{first_error(p)}")

    # Scripts that read stdin and argv, write stderr, exit non-zero, or vary.
    inp = (ROOT / f"{S}/io/input.txt").read_bytes()
    p, v, _ = plain("io-args", ["--stdin", f"{S}/io/input.txt", f"{S}/io/count_words.py", "--", "40"])
    ref = stdout_of(ROOT / f"{S}/io/count_words.py", stdin=inp, args=["40"])
    got = stdout_of(OUT / "stress/io-args/count_words.best.py", stdin=inp, args=["40"])
    record("stdin/argv", "count_words.py --stdin input.txt -- 40", p.returncode == 0 and ref == got,
           f"{outcome(v)}; best.py stdout, stderr and exit code identical with the same stdin/args: {ref == got}")
    p, v, _ = plain("io-exit", ["--stdin", f"{S}/io/input.txt", f"{S}/io/count_words.py", "--", "40", "fail"])
    record("stdin/argv", "count_words.py exiting with status 3", p.returncode == 0 and v["baseline"].get("exit_code") == 3,
           f"baseline exit code {v['baseline'].get('exit_code')} recorded and required of candidates; {outcome(v)}")
    for name, want in [("unseeded_random", "nondeterministic output"), ("prints_time", "nondeterministic output")]:
        p, _, _ = plain(f"io-{name}", [f"{S}/io/{name}.py"])
        record("nondeterminism", f"{name}.py", p.returncode == 1 and want in first_error(p),
               f"exit {p.returncode}: {first_error(p)[:200]}")
    for name in ("seeded_random", "timing_internal"):
        p, v, _ = plain(f"io-{name}", [f"{S}/io/{name}.py"])
        record("nondeterminism", f"{name}.py (deterministic output)", p.returncode == 0 and v["best"]["confirmed"], outcome(v))

    # Encodings and newer syntax.
    for name, check in [("crlf", lambda b: b"\r\n" in b and b.count(b"\n") == b.count(b"\r\n")),
                        ("bom", lambda b: b.startswith(b"\xef\xbb\xbf")),
                        ("tabs", lambda b: b"\n\t" in b)]:
        p, v, _ = plain(f"enc-{name}", [f"{S}/encoding/{name}.py"])
        b = (OUT / f"stress/enc-{name}/{name}.best.py").read_bytes()
        record("encoding", f"{name}.py", p.returncode == 0 and v["best"]["confirmed"] and check(b),
               f"{outcome(v)}; best.py keeps {dict(crlf='the CRLF line endings', bom='the UTF-8 BOM', tabs='tab indentation')[name]}: {check(b)}")
    p, v, _ = plain("enc-unicode", [f"{S}/encoding/unicode_names.py"])
    record("encoding", "unicode_names.py (non-ASCII identifiers and strings)", p.returncode == 0, outcome(v))
    for name in ("latin1", "declared_latin1"):
        p, _, _ = plain(f"enc-{name}", [f"{S}/encoding/{name}.py"])
        record("encoding", f"{name}.py refused with a fix", p.returncode == 2 and "iconv" in first_error(p),
               f"exit {p.returncode}: {first_error(p)[:200]}")
    p, _, _ = plain("syn-pep701", [f"{S}/syntax/pep701.py"])
    record("syntax", "pep701.py (3.12 f-string quote reuse)", p.returncode == 2 and "cannot parse yet" in first_error(p),
           f"exit {p.returncode}: {first_error(p)[:200]}")
    for name in ("pep695", "match_stmt"):
        p, v, _ = plain(f"syn-{name}", [f"{S}/syntax/{name}.py"])
        record("syntax", f"{name}.py parses and runs", p.returncode == 0 and v is not None,
               f"exit 0; baseline {fmt_ns(v['baseline']['median_ns'])}; {outcome(v)}")
    p, v, _ = plain("tiny", [f"{S}/tiny.py"])
    record("too fast", "tiny.py", p.returncode == 0 and "too fast" in v["stop_reason"] and not v["candidates"],
           f"baseline {fmt_ns(v['baseline']['median_ns'])}; stop: {v['stop_reason']}")


def real_model():
    ep = os.environ.get("ACO_VERIFY_ENDPOINT", "http://127.0.0.1:8088")
    model = os.environ.get("ACO_VERIFY_MODEL", "qwen2.5-coder:1.5b")
    (OUT / "stress").mkdir(parents=True, exist_ok=True)
    for name in ("bubble", "lookup", "concat"):
        p, v, secs = plain(f"model-{name}", ["--proposer", "ollama", "--endpoint", ep, "--model", model,
                                             "--gens", "2", f"fixtures/{name}.py"], reps=5)
        if v is None:
            record("real model", f"{model}: fixtures/{name}.py", False, first_error(p))
            continue
        cands = [f"{c['origin'].split('#')[-1]}: {c['decision']}" + (f" ({c['detail'][:40]})" if c.get("detail") else "")
                 for c in v["candidates"]]
        toks = sum(c["tokens"] for c in v["candidates"])
        record("real model", f"{model}: fixtures/{name}.py", p.returncode == 0,
               f"{outcome(v)}{' (flagged for review)' if v['best'].get('model_rewrite') else ''}; "
               f"{len(v['candidates'])} candidates, {toks} tokens, model time "
               f"{v['timings']['propose_ns']/1e9:.1f} s, wall {secs:.0f} s; " + "; ".join(cands))


if __name__ == "__main__":
    main()
