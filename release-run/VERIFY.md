# aco: end-to-end verification

Release build `aco 0.1.0` on this box: Debian 13, Python 3.13.5 at /usr/bin/python3, 8-core Intel Xeon, unshare sandbox (Docker is not installed here; CI covers Docker, see below). Run on 2026-11-02 with `python3 scripts/verify.py` (it writes [verify/results.json](verify/results.json); the per-run transcripts and dashboard screen dumps are next to it).

**Result: 62/62 checks passed.** Every number below was measured in that run; speedups are the confirmation re-run's baseline median over the winner's median (10 interleaved runs each, in fresh sandboxes).

How the checks work:

- **TUI** rows run the real binary in a pseudo-terminal (120x36), render it with the `pyte` terminal emulator, wait for the summary panel, press `q`, and check the exit code, the written files, and that the terminal came back: termios settings identical to before, alternate screen left, cursor shown.
- **--plain / --json** rows pipe stdout (so no colour is expected) and validate each report: `schema_version` 1, required keys and types, every median recomputed from its raw times, confirmation speedup = baseline/candidate, `best.confirmed` = `confirmation.passed`, and an unconfirmed `best_source` equal to the original.
- **best.py == original** runs `python3 original.py` and `python3 <name>.best.py` outside aco and compares stdout byte for byte.
- **cancel** rows also check with `pgrep` that no sandbox process (`aco-sandbox` or the harness) is left running.

| # | Area | Check | Result | Measured / observed |
|---|---|---|---|---|
| 1 | TUI | fixtures/bubble.py | PASS | exit 0; summary panel: 390.3x faster, confirmed; terminal restored: True |
| 2 | TUI | fixtures/concat.py | PASS | exit 0; summary panel: 57.2x faster, confirmed; terminal restored: True |
| 3 | TUI | fixtures/lookup.py | PASS | exit 0; summary panel: 175.2x faster, confirmed; terminal restored: True |
| 4 | TUI | fixtures/slower.py | PASS | exit 0; summary panel: original kept; terminal restored: True |
| 5 | --plain | fixtures/bubble.py | PASS | exit 0, 9s; confirmed 393.3x (rule:bubble-sort:sorted); no ANSI codes in piped output |
| 6 | --json | bubble.report.json | PASS | schema_version 1, keys/types, medians recomputed, speedup = baseline/candidate |
| 7 | best.py == original stdout | bubble | PASS | stdout identical: '16 49681 99992 132208' |
| 8 | --plain | fixtures/concat.py | PASS | exit 0, 9s; confirmed 57.7x (rule:concat:join); no ANSI codes in piped output |
| 9 | --json | concat.report.json | PASS | schema_version 1, keys/types, medians recomputed, speedup = baseline/candidate |
| 10 | best.py == original stdout | concat | PASS | stdout identical: '195522 0,31,62,93,124,155,186,2 193,3224,3255,3286,3317,' |
| 11 | --plain | fixtures/lookup.py | PASS | exit 0, 7s; confirmed 164.5x (rule:membership:set); no ANSI codes in piped output |
| 12 | --json | lookup.report.json | PASS | schema_version 1, keys/types, medians recomputed, speedup = baseline/candidate |
| 13 | best.py == original stdout | lookup | PASS | stdout identical: '2000' |
| 14 | --plain | fixtures/slower.py | PASS | exit 0, 3s; original kept; no ANSI codes in piped output |
| 15 | --json | slower.report.json | PASS | schema_version 1, keys/types, medians recomputed, speedup = baseline/candidate |
| 16 | best.py == original stdout | slower | PASS | stdout identical: '800000' |
| 17 | slower.py rejected | slower.py | PASS | rule:membership:set 328.6 ms (0.67x) reject_margin; rule:membership:frozenset 295.4 ms (0.75x) reject_margin vs baseline 221.1 ms; best.py is the original |
| 18 | repeatability | bubble | PASS | confirmed 3/3: 393.3x, 389.8x, 402.2x (spread 3% of mean) |
| 19 | repeatability | concat | PASS | confirmed 3/3: 57.7x, 57.9x, 55.5x (spread 4% of mean) |
| 20 | repeatability | lookup | PASS | confirmed 3/3: 164.5x, 179.7x, 179.8x (spread 9% of mean) |
| 21 | repeatability | slower | PASS | rejected 3/3 |
| 22 | search controls | --gens 1 stops after one generation | PASS | generations 1, evaluated per gen [2], stop: reached --gens 1 |
| 23 | search controls | --pop 2 measures 1 candidate per generation | PASS | generations 2, evaluated per gen [1, 0], stop: search exhausted: the proposer has no new candidates for the elite |
| 24 | search controls | --pop 4 measures both rule rewrites (rules offer 2) | PASS | generations 2, evaluated per gen [2, 0], stop: search exhausted: the proposer has no new candidates for the elite |
| 25 | search controls | --patience 3 on slower.py (rules exhaust first) | PASS | generations 2, stop: search exhausted: the proposer has no new candidates for the elite; patience itself is covered by tests/search_fake.rs (no_improvement_stops_after_patience) |
| 26 | bad input | missing file | PASS | exit 2: error: `nope.py` does not exist |
| 27 | bad input | syntax error | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/syntax.py` is not valid Python: invalid syntax. Got unexpected token ':' |
| 28 | bad input | empty file | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/empty.py` has no code to optimise (it is empty or only comments) |
| 29 | bad input | comments only | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/comments.py` has no code to optimise (it is empty or only comments) |
| 30 | bad input | non-Python text | PASS | exit 2: error: `README.md` is not valid Python: Got unexpected token ` |
| 31 | bad input | binary file | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/blob.py` is not UTF-8 text |
| 32 | bad input | directory | PASS | exit 2: error: `fixtures` is a directory |
| 33 | policy | policy: import os | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_os.py` was rejected before running: it imports banned module `os` |
| 34 | policy | policy: subprocess | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_sub.py` was rejected before running: it imports banned module `subprocess` |
| 35 | policy | policy: open() | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_open.py` was rejected before running: it uses banned builtin `open` |
| 36 | policy | policy: eval() | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_eval.py` was rejected before running: it uses banned builtin `eval` |
| 37 | policy | policy: socket | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_sock.py` was rejected before running: it imports banned module `socket` |
| 38 | policy | policy: __import__ | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_imp.py` was rejected before running: it uses banned builtin `__import__` |
| 39 | policy | policy: dunder escape | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_dunder.py` was rejected before running: it accesses dunder attribute `.__subclasses__` |
| 40 | sandbox | sandbox timeout | PASS | exit 1: error: /workspace/aco/release-run/verify/inputs/spin.py: baseline failed in the sandbox: timed out |
| 41 | sandbox | sandbox memory limit | PASS | exit 1: error: /workspace/aco/release-run/verify/inputs/hog.py: baseline failed in the sandbox: MemoryError:  |
| 42 | bad input | crashing script | PASS | exit 1: error: /workspace/aco/release-run/verify/inputs/crash.py: baseline failed in the sandbox: ZeroDivisionError: division by zero |
| 43 | bad input | no sandbox (docker absent) | PASS | exit 3: error: no sandbox is available, so aco will not run any code |
| 44 | bad input | usage: --reps 4 | PASS | exit 2: error: invalid value '4' for '--reps <REPS>': must be odd so the median is a real measurement |
| 45 | bad input | usage: --pop 1 | PASS | exit 2: error: invalid value '1' for '--pop <POP>': must be at least 2 (the current best plus one candidate) |
| 46 | model proposer | mock server: good | PASS | mock reply kept and confirmed at 178.3x, tokens counted 42 |
| 47 | model proposer | mock server: policy | PASS | exit 0, original kept; note: model reply 0 dropped by policy: imports banned module `os` |
| 48 | model proposer | mock server: wrong | PASS | a reply that prints something else is measured and rejected: output differs |
| 49 | model proposer | mock server: garbage | PASS | exit 0, original kept; note: model request to http://127.0.0.1:45587 failed: json: expected ident at line 1 column 2 |
| 50 | model proposer | mock server: 500 | PASS | exit 0, original kept; note: model request to http://127.0.0.1:45587 failed: http status: 500 |
| 51 | model proposer | refused (nothing listening) | PASS | exit 0 in 1.0s; note: model request to http://127.0.0.1:9 failed: io: Connection refused (os error 111) (is Ollama or llama-server running at http://127.0.0.1:9?) |
| 52 | model proposer | unroutable address | PASS | exit 0 in 11.1s; note: model request to http://10.255.255.1:11434 failed: timeout: connect (is Ollama or llama-server running at http://10.255.255.1:11434?) |
| 53 | cancel | Ctrl-C in the dashboard mid-search | PASS | exit 130 0.3s after the action; termios restored: True; alt-screen left + cursor shown: True; leftover sandbox processes: 0 |
| 54 | cancel | q in the dashboard mid-search | PASS | exit 130 0.3s after the action; termios restored: True; alt-screen left + cursor shown: True; leftover sandbox processes: 0 |
| 55 | cancel | Ctrl-C during confirmation | PASS | exit 130 0.3s after the action; termios restored: True; alt-screen left + cursor shown: True; leftover sandbox processes: 0 |
| 56 | cancel | SIGTERM to the dashboard | PASS | exit 130 0.3s after the action; termios restored: True; alt-screen left + cursor shown: True; leftover sandbox processes: 0 |
| 57 | cancel | Ctrl-C in --plain (SIGINT from the tty) | PASS | exit 130 0.2s after the action; termios restored: True; alt-screen left + cursor shown: True; leftover sandbox processes: 0 |
| 58 | cancel | panic in the search thread (ACO_TEST_PANIC=1) | PASS | exit 101 0.3s after the action; termios restored: True; alt-screen left + cursor shown: True; leftover sandbox processes: 0 |
| 59 | resize | 120x36 → 100x30 → 60x18 → 140x42 mid-run | PASS | 'terminal too small' shown at 60x18: True; final 140x42 frame has summary and diff: True; exit 0 |
| 60 | colour | --plain on a TTY is coloured | PASS | SGR codes present |
| 61 | colour | NO_COLOR=1 on a TTY | PASS | no escape codes at all |
| 62 | colour | stdout piped (no --plain) | PASS | falls back to line output, no escape codes |

## Sandbox isolation (runner-level, release build)

The CLI's policy pre-filter refuses `os`, `socket`, `open` and friends before anything runs (rows above), so isolation is tested one level down, by running hostile snippets straight through the runner. `cargo test --release --test sandbox --test abort -- --test-threads 1` on this box:

| Test | unshare (this box) | Docker |
|---|---|---|
| host env var set in the parent is not visible inside | pass | ran and passed in CI on ubuntu-latest; skipped here (no Docker) |
| a file under the repo and the host's /etc/hostname cannot be read | pass | ran and passed in CI on ubuntu-latest; skipped here (no Docker) |
| TCP connects to 1.1.1.1:53 and 8.8.8.8:53 fail | pass | ran and passed in CI on ubuntu-latest; skipped here (no Docker) |
| `while True: pass` is killed at the wall-clock limit | pass | ran and passed in CI on ubuntu-latest; skipped here (no Docker) |
| times come from inside (median < 20 ms for a tiny body, startup excluded) | pass | ran and passed in CI on ubuntu-latest; skipped here (no Docker) |
| 1 GiB allocation fails under a 256 MiB limit; 32 MiB still works | pass | ran and passed in CI on ubuntu-latest; skipped here (no Docker) |
| `abort_all` (what Ctrl-C calls) kills a running sandbox | pass in 1.5 s | ran in CI with Docker |
| unshare refuses to run when user namespaces are off, naming both sysctls | pass | n/a |

Through the CLI, the same limits show up as clean errors: an infinite loop with `--timeout 2` and a 512 MiB allocation with `--memory-mb 128` both exit 1 with `baseline failed in the sandbox: timed out` / `MemoryError` (rows above).

## CI (GitHub Actions, ubuntu-latest)

At commit 7ae0f00 CI was green: `cargo fmt --check`, clippy with `-D warnings`, and `cargo test -- --nocapture` (so skips are visible in the log). Docker 28.0.4 was available, and all six Docker isolation tests, including the memory limit, **ran and passed**. The real-runner search tests ran through Docker and kept and confirmed bubble, concat and lookup. The unshare tests **skipped**, as expected: `kernel.apparmor_restrict_unprivileged_userns = 1` on Ubuntu 24.04 makes `unshare` fail with `write failed /proc/self/uid_map: Operation not permitted`. aco reports that and refuses to run, rather than falling back to running unsandboxed.

The CI end-to-end step ran the debug binary with `--sandbox docker --reps 3` on lookup.py and slower.py (CPython 3.12 in `python:3.12-slim`):

- lookup.py: baseline 646.2 ms, candidate 2.722 ms, **237.4x, confirmed**
- slower.py: baseline 549.3 ms, both rewrites rejected, original kept

## Search controls

The built-in rules offer at most two rewrites per fixture and are deterministic, so on real runs `--gens`/`--pop` are observable but `--patience` never gets a chance to fire: the search stops as soon as the rules have nothing new ("search exhausted"). `--patience`, `--gens` on a search that keeps improving, and `--pop` with 10 proposals per generation are covered with scripted proposers in `tests/search_fake.rs` (`no_improvement_stops_after_patience`, `gens_caps_a_search_that_keeps_improving`, `pop_bounds_candidates_per_generation`).

## Notes

- The model-proposer rows use a local mock OpenAI-compatible server (no model weights). The "good" reply is a hand-written set rewrite of lookup.py; it was measured and confirmed like any rule rewrite.
- `ACO_TEST_PANIC=1` is a test-only hook that panics in the search thread, to prove the terminal is restored on a crash (exit 101).
- Speedups depend on the interpreter. CI's Docker image runs CPython 3.12, where concat.py's baseline is about 47 ms instead of about 258 ms here, so its speedup there is much smaller.
- [bench.md](bench.md) and [transcript.txt](transcript.txt) are from the first release run (before this UX pass); their speedups (392.6x, 57.4x, 184.1x) are in the same range as the ones above.
