#!/usr/bin/env python3
"""Release-run bench: runs the release binary on the core fixtures and the
stress cases with --plain --json and writes release-run/transcript.txt (the
real output), release-run/bench.md and release-run/bench.json from the reports.

Usage: cargo build --release && python3 scripts/bench.py
"""
import datetime
import json
import os
import platform
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target/release/aco"
REL = ROOT / "release-run"
S = "fixtures/stress"

CORE = ["fixtures/bubble.py", "fixtures/concat.py", "fixtures/lookup.py", "fixtures/slower.py"]
STRESS = [
    ("large_300.py (300 lines)", [f"{S}/large_300.py"]),
    ("large_1000.py (1,000 lines)", [f"{S}/large_1000.py"]),
    ("large_3000.py (3,000 lines)", [f"{S}/large_3000.py"]),
    ("project, 6 files, --mutate shop/inventory.py", ["--mutate", f"{S}/project/shop/inventory.py", f"{S}/project/main.py"]),
    ("numpy_project (numpy 2.3.4 + more-itertools)", [f"{S}/numpy_project/stats.py"]),
    ("stdlib_heavy.py", [f"{S}/stdlib_heavy.py"]),
    ("traps/duplicates_order.py", [f"{S}/traps/duplicates_order.py"]),
    ("traps/set_output.py (prints a set)", [f"{S}/traps/set_output.py"]),
    ("traps/alias_set.py (unsound to rewrite)", [f"{S}/traps/alias_set.py"]),
    ("io/count_words.py --stdin input.txt -- 40", ["--stdin", f"{S}/io/input.txt", f"{S}/io/count_words.py", "--", "40"]),
    ("io/seeded_random.py", [f"{S}/io/seeded_random.py"]),
    ("encoding/crlf.py (CRLF kept)", [f"{S}/encoding/crlf.py"]),
    ("encoding/bom.py (BOM kept)", [f"{S}/encoding/bom.py"]),
    ("syntax/match_stmt.py", [f"{S}/syntax/match_stmt.py"]),
    ("tiny.py (2 µs)", [f"{S}/tiny.py"]),
]


def sp(r):
    return f"{r['speedup']:.1f}x" if r["speedup"] else "-"


def ms(ns):
    if ns is None:
        return "-"
    return f"{ns / 1e6:.3f} ms" if ns >= 1e6 else f"{ns / 1e3:.1f} µs"


def run(args, out, log):
    cmd = [str(BIN), "--plain", "--color", "never", "--json", "--out", str(out), *args]
    shown = "aco " + " ".join(cmd[1:]).replace(str(ROOT) + "/", "")
    p = subprocess.run(cmd, capture_output=True, text=True, cwd=ROOT,
                       env=dict(os.environ, ACO_CACHE_DIR=str(ROOT / "target/aco-cache")))
    log.append(f"$ {shown}\n{p.stdout}{p.stderr}(exit {p.returncode})\n")
    return p


def report_for(out, args):
    target = Path(args[-1] if "--" not in args else args[args.index("--") - 1])
    mutated = Path(args[args.index("--mutate") + 1]) if "--mutate" in args else target
    return json.loads((out / f"{mutated.stem}.report.json").read_text())


def row(label, v):
    b, c, t = v["best"], v.get("confirmation"), v["timings"]
    files = len(v["run"]["files"])
    lines = sum(f["lines"] for f in v["run"]["files"])
    return {
        "case": label, "files": files, "lines": lines,
        "packages": [f"{p['name']} {p['version']}" for p in v["run"]["packages"]],
        "baseline_ns": v["baseline"]["median_ns"],
        "confirm_baseline_ns": c and c["baseline_median_ns"],
        "best_ns": c and c["candidate_median_ns"],
        "speedup": b["speedup"] if b["confirmed"] else None,
        "wins": c and f"{c['candidate_wins']}/{c['rounds']}",
        "winner": b["origin"] if b["confirmed"] else None,
        "candidates": len(v["candidates"]),
        "result": "confirmed" if b["confirmed"] else
                  ("too fast to measure" if v["stop_reason"].startswith("too fast") else b["reason"]),
        "parse_ms": t["parse_ns"] / 1e6, "rules_ms": t["propose_ns"] / 1e6,
        "deps_ms": t["deps_ns"] / 1e6, "search_s": t["search_ns"] / 1e9,
    }


def main():
    if not BIN.exists():
        sys.exit("build first: cargo build --release")
    log = []
    cpu = os.cpu_count()
    py = subprocess.run(["python3", "--version"], capture_output=True, text=True).stdout.strip()
    stamp = datetime.datetime.now().astimezone().strftime("%Y-%m-%d %H:%M %Z")
    log.append(f"# {stamp} on {platform.system()} {platform.release()}, {py}, {cpu} CPUs, "
               f"{subprocess.run([str(BIN), '--version'], capture_output=True, text=True).stdout.strip()}\n")
    p = run(CORE, REL, log)
    core = [row(Path(f).name, json.loads((REL / f"{Path(f).stem}.report.json").read_text())) for f in CORE]
    stress = []
    for label, args in STRESS:
        out = REL / "stress"
        run(args, out, log)
        stress.append(row(label, report_for(out, args)))
    (REL / "transcript.txt").write_text("\n".join(log))
    (REL / "bench.json").write_text(json.dumps({"when": stamp, "python": py, "cpus": cpu,
                                                "core": core, "stress": stress}, indent=2) + "\n")

    md = [
        "# aco release run: bench", "",
        f"Measured {stamp} with the release build on this box ({py}, {cpu} CPUs, unshare sandbox), "
        "`--plain --json`, rules proposer, default settings (5 reps after 1 warmup, 5% margin, 3 generations). "
        "Times are medians measured inside the sandbox by the harness; sandbox startup and package installs are not timed. "
        "Confirmation re-runs baseline and winner interleaved in fresh sandboxes, 10 rounds, with `PYTHONHASHSEED` 1-10. "
        "The full output is [transcript.txt](transcript.txt); numbers come from the `*.report.json` files "
        "([bench.json](bench.json) has them all). Regenerate with `python3 scripts/bench.py`.", "",
        "## Fixtures", "",
        "| Fixture | Baseline (search) | Confirm: baseline | Confirm: winner | Speedup | Rounds won | Winner | Result |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for r in core:
        md.append(f"| {r['case']} | {ms(r['baseline_ns'])} | {ms(r['confirm_baseline_ns'])} | {ms(r['best_ns'])} | "
                  f"{sp(r)} | {r['wins'] or '-'} | {r['winner'] or '(original kept)'} | {r['result']} |")
    md += ["", "## Stress and edge cases", "",
           "| Case | Files / lines | Packages | Baseline | Winner median | Speedup | Rounds won | Result | Parse | Rules | Install | Search |",
           "|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in stress:
        spd = sp(r)
        md.append(f"| {r['case']} | {r['files']} / {r['lines']} | {', '.join(r['packages']) or '-'} | "
                  f"{ms(r['confirm_baseline_ns'] or r['baseline_ns'])} | {ms(r["best_ns"]) if r["speedup"] else "-"} | {spd} | "
                  f"{r['wins'] or '-'} | {r['result']} | {r['parse_ms']:.1f} ms | {r['rules_ms']:.0f} ms | "
                  f"{r['deps_ms']:.0f} ms | {r['search_s']:.1f} s |")
    md += ["", "Install is the package step (cached here after the first run; a cold numpy install measured "
           "1.0 s in release-run/VERIFY.md). Rules is the time the rewrite rules took to analyse the file. "
           "Baseline is the confirmation baseline when a confirmation ran, else the search-phase median.", ""]
    (REL / "bench.md").write_text("\n".join(md))
    print("\n".join(md))


if __name__ == "__main__":
    main()
