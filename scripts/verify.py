#!/usr/bin/env python3
"""End-to-end verification of the release build.

Runs the real binary on the fixtures (live dashboard in a pseudo-terminal,
--plain, --json), checks outputs, cancellation, resizing, colour, bad inputs,
the model proposer against mock and unreachable endpoints, and repeats the
confirmed speedups. Writes release-run/verify/results.json and artefacts.

Usage: python3 scripts/verify.py   (after `cargo build --release`)
Needs: python3, pyte (pip install pyte) for the terminal checks.
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
        ("binary file", [str(bad / "blob.py")], 2, "is not UTF-8 text"),
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

    (OUT / "results.json").write_text(json.dumps({"version": version, "python": py, "results": results}, indent=2) + "\n")
    failed = [r for r in results if not r["pass"]]
    print(f"\n{len(results) - len(failed)}/{len(results)} passed", flush=True)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
