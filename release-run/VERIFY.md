# aco: end-to-end verification

Release build `aco 0.1.0` on this box: Debian 13, Python 3.13.5 at /usr/bin/python3, 8-core Intel Xeon, unshare sandbox (Docker is not installed here; CI covers Docker, see below). Run on 2026-11-02 with `python3 scripts/verify.py core stress model` (it writes [verify/results.json](verify/results.json); per-run transcripts, reports and dashboard screen dumps are next to it, the stress ones under `verify/stress/`).

**Result: 103/103 checks passed.** Every number below was measured in that run; speedups are the confirmation re-run's baseline median over the winner's median (interleaved runs in fresh sandboxes, `2 x reps` rounds, `PYTHONHASHSEED` 1..N; the stress rows use `--reps 3`, so 6 rounds).

How the checks work:

- **TUI** rows run the real binary in a pseudo-terminal (120x36), render it with the `pyte` terminal emulator, wait for the summary panel, press `q`, and check the exit code, the written files, and that the terminal came back: termios settings identical to before, alternate screen left, cursor shown.
- **--plain / --json** rows pipe stdout (so no colour is expected) and validate each report: `schema_version` 1, required keys and types, every median recomputed from its raw times, confirmation speedup = baseline/candidate, `best.confirmed` = `confirmation.passed`, and an unconfirmed `best_source` equal to the original.
- **best.py == original** runs `python3 original.py` and `python3 <name>.best.py` outside aco and compares stdout byte for byte. The project row copies the project, drops `inventory.best.py` in, and compares the project's output.
- **cancel** and **hang-up** rows also check with `pgrep` that no sandbox process (`aco-sandbox` or the harness) is left running. The hang-up rows close the pseudo-terminal under a finished dashboard, once as the controlling terminal (SIGHUP) and once as plain stdin/stdout (no SIGHUP).
- **Stress** rows use the generated cases in [fixtures/stress](../fixtures/stress) ([scripts/gen_stress.py](../scripts/gen_stress.py)), a 400-module project and a 1,003-module closure generated at run time, a package with no wheel (pycairo 1.20.0), and a mock model server for the hash-order trap. Package rows install from PyPI into a fresh cache.
- **real model** rows run the search against Qwen2.5-Coder-1.5B-Instruct (Q4_K_M GGUF, 1.1 GB) served by llama.cpp b12034's `llama-server` on this box's CPU (8 threads, about 41 tokens/s), `--gens 2`. The model samples, so its candidates differ from run to run.

| # | Area | Check | Result | Measured / observed |
|---|---|---|---|---|
| 1 | TUI | fixtures/bubble.py | PASS | exit 0; summary panel: 406.1x faster, confirmed; terminal restored: True |
| 2 | TUI | fixtures/concat.py | PASS | exit 0; summary panel: 55.9x faster, confirmed; terminal restored: True |
| 3 | TUI | fixtures/lookup.py | PASS | exit 0; summary panel: 165.6x faster, confirmed; terminal restored: True |
| 4 | TUI | fixtures/slower.py | PASS | exit 0; summary panel: original kept; terminal restored: True |
| 5 | --plain | fixtures/bubble.py | PASS | exit 0, 9s; confirmed 396.6x (rule:bubble-sort:list.sort); no ANSI codes in piped output |
| 6 | --json | bubble.report.json | PASS | schema_version 1, keys/types, medians recomputed, speedup = baseline/candidate |
| 7 | best.py == original stdout | bubble | PASS | stdout identical: '16 49681 99992 132208' |
| 8 | --plain | fixtures/concat.py | PASS | exit 0, 9s; confirmed 56.3x (rule:concat:join); no ANSI codes in piped output |
| 9 | --json | concat.report.json | PASS | schema_version 1, keys/types, medians recomputed, speedup = baseline/candidate |
| 10 | best.py == original stdout | concat | PASS | stdout identical: '195522 0,31,62,93,124,155,186,2 193,3224,3255,3286,3317,' |
| 11 | --plain | fixtures/lookup.py | PASS | exit 0, 7s; confirmed 178.9x (rule:membership:set); no ANSI codes in piped output |
| 12 | --json | lookup.report.json | PASS | schema_version 1, keys/types, medians recomputed, speedup = baseline/candidate |
| 13 | best.py == original stdout | lookup | PASS | stdout identical: '2000' |
| 14 | --plain | fixtures/slower.py | PASS | exit 0, 4s; original kept; no ANSI codes in piped output |
| 15 | --json | slower.report.json | PASS | schema_version 1, keys/types, medians recomputed, speedup = baseline/candidate |
| 16 | best.py == original stdout | slower | PASS | stdout identical: '800000' |
| 17 | slower.py rejected | slower.py | PASS | rule:membership:set 337.5 ms (0.70x) reject_margin; rule:membership:frozenset 344.5 ms (0.68x) reject_margin vs baseline 235.0 ms; best.py is the original |
| 18 | repeatability | bubble | PASS | confirmed 3/3: 396.6x, 394.7x, 385.5x (spread 3% of mean) |
| 19 | repeatability | concat | PASS | confirmed 3/3: 56.3x, 57.4x, 57.3x (spread 2% of mean) |
| 20 | repeatability | lookup | PASS | confirmed 3/3: 178.9x, 171.8x, 173.6x (spread 4% of mean) |
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
| 31 | bad input | binary file | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/blob.py` cannot be read as Python source: it is not text (it contains NUL bytes) |
| 32 | bad input | directory | PASS | exit 2: error: `fixtures` is a directory |
| 33 | policy | policy: import os | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_os.py` was rejected before running: line 1 imports banned module `os` |
| 34 | policy | policy: subprocess | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_sub.py` was rejected before running: line 1 imports banned module `subprocess` |
| 35 | policy | policy: open() | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_open.py` was rejected before running: line 1 uses banned builtin `open` |
| 36 | policy | policy: eval() | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_eval.py` was rejected before running: line 1 uses banned builtin `eval` |
| 37 | policy | policy: socket | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_sock.py` was rejected before running: line 1 imports banned module `socket` |
| 38 | policy | policy: __import__ | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_imp.py` was rejected before running: line 1 uses banned builtin `__import__` |
| 39 | policy | policy: dunder escape | PASS | exit 2: error: `/workspace/aco/release-run/verify/inputs/p_dunder.py` was rejected before running: line 1 accesses dunder attribute `.__subclasses__` |
| 40 | sandbox | sandbox timeout | PASS | exit 1: error: /workspace/aco/release-run/verify/inputs/spin.py: baseline failed in the sandbox: timed out |
| 41 | sandbox | sandbox memory limit | PASS | exit 1: error: /workspace/aco/release-run/verify/inputs/hog.py: baseline failed in the sandbox: MemoryError:  |
| 42 | bad input | crashing script | PASS | exit 1: error: /workspace/aco/release-run/verify/inputs/crash.py: baseline failed in the sandbox: ZeroDivisionError: division by zero |
| 43 | bad input | no sandbox (docker absent) | PASS | exit 3: error: no sandbox is available, so aco will not run any code |
| 44 | bad input | usage: --reps 4 | PASS | exit 2: error: invalid value '4' for '--reps <REPS>': must be odd so the median is a real measurement |
| 45 | bad input | usage: --pop 1 | PASS | exit 2: error: invalid value '1' for '--pop <POP>': must be at least 2 (the current best plus one candidate) |
| 46 | model proposer | mock server: good | PASS | mock reply kept and confirmed at 172.7x, tokens counted 42 |
| 47 | model proposer | mock server: policy | PASS | exit 0, original kept; note: model reply 0 dropped by policy: imports banned module `os` |
| 48 | model proposer | mock server: wrong | PASS | a reply that prints something else is measured and rejected: output differs |
| 49 | model proposer | mock server: garbage | PASS | exit 0, original kept; note: model request to http://127.0.0.1:43665 failed: json: expected ident at line 1 column 2 |
| 50 | model proposer | mock server: 500 | PASS | exit 0, original kept; note: model request to http://127.0.0.1:43665 failed: http status: 500 |
| 51 | model proposer | refused (nothing listening) | PASS | exit 0 in 1.0s; note: model request to http://127.0.0.1:9 failed: io: Connection refused (os error 111) (is Ollama or llama-server running at http://127.0.0.1:9?) |
| 52 | model proposer | unroutable address | PASS | exit 0 in 11.0s; note: model request to http://10.255.255.1:11434 failed: timeout: connect (is Ollama or llama-server running at http://10.255.255.1:11434?) |
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
| 63 | hang-up | controlling terminal closed after the run (SIGHUP) | PASS | exited 0.06s after the terminal closed (exit 130); leftover sandbox processes: 0 |
| 64 | hang-up | stdin/stdout terminal closed, no SIGHUP | PASS | exited 0.06s after the terminal closed (exit 130); leftover sandbox processes: 0 |
| 65 | large file | large_300.py (300 lines) | PASS | confirmed 21.4x (55.1 ms → 2.6 ms); 7 candidates; parse 0.5 ms, policy 0.2 ms, rewrite rules 14 ms, search 2.0 s; wall 2.1 s; best.py stdout identical: True |
| 66 | large file | large_1000.py (1000 lines) | PASS | confirmed 23.9x (55.4 ms → 2.3 ms); 7 candidates; parse 1.6 ms, policy 0.5 ms, rewrite rules 44 ms, search 2.1 s; wall 2.2 s; best.py stdout identical: True |
| 67 | large file | large_3000.py (3000 lines) | PASS | confirmed 21.8x (57.4 ms → 2.6 ms); 7 candidates; parse 5.4 ms, policy 1.6 ms, rewrite rules 137 ms, search 2.6 s; wall 2.8 s; best.py stdout identical: True |
| 68 | project | main.py imports 5 local modules (package, relative imports) | PASS | 6 files copied in (main.py, helpers.py, shop/__init__.py, shop/inventory.py, shop/pricing.py, shop/util.py); note: the rules find nothing to rewrite in main.py, but they do in shop/inventory.py; only one file is rewritten per run, so t |
| 69 | project | --mutate shop/inventory.py | PASS | confirmed 21.4x (53.2 ms → 2.5 ms); project stdout with inventory.best.py dropped in identical: True; source files unchanged: True |
| 70 | project | generated project: 400 modules (20 import chains of 20), hot spot in pkg/m210.py | PASS | 402 files; import analysis 7 ms; confirmed 9.8x (224.8 ms → 22.9 ms); wall 4.6 s |
| 71 | project | 1003-module closure is refused (cap 1000 files) | PASS | exit 2: error: the project is too large for the sandbox: the target imports more than 1000 local files or 20 MB of source. Point --project-root at a smaller directory, or move generated or vendored code out of it |
| 72 | stdlib | stdlib_heavy.py (collections, itertools, heapq, re, json, dataclasses …) | PASS | exit 0; original kept; baseline 37.3 ms |
| 73 | packages | numpy_project (numpy + more-itertools), fresh cache | PASS | confirmed 50.8x (219.9 ms → 4.3 ms); installed more-itertools 10.5.0, numpy 2.3.4; install 1.0 s (outside the timed runs); wall 6.2 s |
| 74 | packages | numpy_project rerun reuses the cache | PASS | deps step 11 ms; confirmed 45.0x (199.8 ms → 4.4 ms) |
| 75 | packages | import of an undeclared package | PASS | exit 2: error: missing_dep.py:1: imports `yaml`, which is not in the standard library, the project, or the installed requirements. Add `PyYAML` to a requirements.txt next to the script (or pass --requirements FILE) |
| 76 | packages | requirements.txt with a pip option (-e git+…) | PASS | exit 2: error: requirements: fixtures/stress/bad_requirements/requirements.txt:2: `-e git+https://example.com/repo.git#egg=thing`: pip options are not supported (aco installs plain PyPI requirements as wheels only) |
| 77 | packages | package with no wheel (pycairo 1.20.0, sdist only) | PASS | exit 2: error: could not install requirements from /tmp/aco-nowheel-0yg3e0ko/requirements.txt: `pycairo==1.20.0` has no installable wheel for this interpreter (3.13.5 cpython-313 linux-x86_64). aco installs wheels only (`--only- |
| 78 | semantics trap | alias_set.py: list mutated through an alias after the loop | PASS | no rewrite proposed (rule refuses statically); 0 candidates; original kept (at 454c873 the rule fired and only the output check caught it) |
| 79 | semantics trap | global_mutation.py: list mutated by a function through `global` | PASS | no rewrite proposed (rule refuses statically); 0 candidates; original kept (at 454c873 the rule fired and only the output check caught it) |
| 80 | semantics trap | partial_bubble.py: bubble loop that stops early (not a full sort) | PASS | no rewrite proposed (rule refuses statically); 0 candidates; original kept (at 454c873 the rule fired and only the output check caught it) |
| 81 | semantics trap | loopvar_bubble.py: loop variable read after the sort | PASS | no rewrite proposed (rule refuses statically); 0 candidates; original kept (at 454c873 the rule fired and only the output check caught it) |
| 82 | semantics trap | duplicates_order.py: order-preserving dedup keeps order | PASS | confirmed 143.6x (91.5 ms → 637.1 µs); best.py stdout identical: True |
| 83 | semantics trap | set_output.py prints a set (hash-order output) | PASS | confirmed 1.7x (12.1 ms → 7.2 ms); baseline and candidate share each round's PYTHONHASHSEED, so a correct rewrite is accepted (at 454c873 random per-process seeds rejected it as 'output differs') |
| 84 | semantics trap | hashseed.py: list(set(…)) matches only under seed 0 | PASS | passed the search (seed 0) at 9x, then confirmation: output differed from the original with PYTHONHASHSEED=1 (it depends on hash or iteration order) |
| 85 | stdin/argv | count_words.py --stdin input.txt -- 40 | PASS | original kept; best.py stdout, stderr and exit code identical with the same stdin/args: True |
| 86 | stdin/argv | count_words.py exiting with status 3 | PASS | baseline exit code 3 recorded and required of candidates; confirmed 1.1x (26.2 ms → 23.6 ms) |
| 87 | nondeterminism | unseeded_random.py | PASS | exit 1: error: fixtures/stress/io/unseeded_random.py: baseline failed in the sandbox: nondeterministic output: stdout, stderr or the exit code differed between repetitions of the same code (printing random nu |
| 88 | nondeterminism | prints_time.py | PASS | exit 1: error: fixtures/stress/io/prints_time.py: baseline failed in the sandbox: nondeterministic output: stdout, stderr or the exit code differed between repetitions of the same code (printing random number |
| 89 | nondeterminism | seeded_random.py (deterministic output) | PASS | confirmed 200.6x (110.3 ms → 549.8 µs) |
| 90 | nondeterminism | timing_internal.py (deterministic output) | PASS | confirmed 3.2x (7.9 ms → 2.5 ms) |
| 91 | encoding | crlf.py | PASS | confirmed 1141.1x (143.0 ms → 125.3 µs); best.py keeps the CRLF line endings: True |
| 92 | encoding | bom.py | PASS | confirmed 1113.5x (144.9 ms → 130.1 µs); best.py keeps the UTF-8 BOM: True |
| 93 | encoding | tabs.py | PASS | confirmed 603.2x (80.5 ms → 133.5 µs); best.py keeps tab indentation: True |
| 94 | encoding | unicode_names.py (non-ASCII identifiers and strings) | PASS | confirmed 3.0x (2.9 ms → 982.7 µs) |
| 95 | encoding | latin1.py refused with a fix | PASS | exit 2: error: `fixtures/stress/encoding/latin1.py` cannot be read as Python source: it is not UTF-8 text (byte 0xE9 on line 1). If it is Latin-1 or cp1252, convert it with `iconv -f cp1252 -t utf-8 FILE > FI |
| 96 | encoding | declared_latin1.py refused with a fix | PASS | exit 2: error: `fixtures/stress/encoding/declared_latin1.py` cannot be read as Python source: it declares `coding: latin-1`; aco only reads UTF-8 source. Convert it with `iconv -f latin-1 -t utf-8 FILE > FILE |
| 97 | syntax | pep701.py (3.12 f-string quote reuse) | PASS | exit 2: error: `fixtures/stress/syntax/pep701.py` uses Python syntax aco cannot parse yet (line 2: f-string: expecting '}') |
| 98 | syntax | pep695.py parses and runs | PASS | exit 0; baseline 2.5 µs; original kept |
| 99 | syntax | match_stmt.py parses and runs | PASS | exit 0; baseline 2.4 ms; confirmed 1.5x (2.4 ms → 1.6 ms) |
| 100 | too fast | tiny.py | PASS | baseline 2.1 µs; stop: too fast to measure: baseline median 2.1 µs is under 1.000 ms |
| 101 | real model | qwen2.5-coder:1.5b: fixtures/bubble.py | PASS | original kept; 3 candidates, 429 tokens, model time 11.7 s, wall 16 s; 0: reject_error (nondeterministic output: stdout, stderr ); 1: reject_error (NameError: name 'bubble' is not defined); 2: reject_margin |
| 102 | real model | qwen2.5-coder:1.5b: fixtures/lookup.py | PASS | confirmed 12153.3x (244.9 ms → 20.2 µs) (flagged for review); 4 candidates, 193 tokens, model time 7.9 s, wall 15 s; 0: keep; 1: keep; 2: keep; 1: reject_margin |
| 103 | real model | qwen2.5-coder:1.5b: fixtures/concat.py | PASS | original kept; 3 candidates, 189 tokens, model time 4.8 s, wall 8 s; 0: reject_margin; 1: reject_margin; 2: reject_error (TypeError: can only concatenate str (not) |

## Before and after this pass (same fixtures, old binary at 454c873)

| Case | 454c873 | Now |
|---|---|---|
| traps/alias_set.py (list mutated through an alias) | set rule fired, 25.4x, caught only by the output check ("output differs") | rule refuses statically; nothing unsound is proposed |
| traps/global_mutation.py (list mutated via `global`) | set rule fired, 42.3x, "output differs" | refused statically |
| traps/partial_bubble.py (early-exit loop, not a full sort) | bubble rule fired, 11.8x, "output differs" | refused statically |
| traps/loopvar_bubble.py (loop variable read after the sort) | bubble rule fired, crashed: `NameError: name 'i' is not defined` | refused statically |
| traps/set_output.py (prints a set; correct rewrite) | correct rewrite rejected, "output differs" (each run had its own random hash seed) | confirmed 1.6-1.7x; baseline and candidate share each round's seed |
| traps/hashseed.py (`list(set(...))` right only under one seed) | output compared under random seeds, no shared-seed check | passes the seed-0 search, then rejected in confirmation at `PYTHONHASHSEED=1` |
| io/count_words.py (`import sys` for argv/stdin) | refused: "imports banned module `sys`" | runs, with `--stdin` and args after `--` |
| `def f(a=open("/etc/hostname"))` | not caught by the pre-filter (the sandbox contained it: FileNotFoundError) | refused: "line 1 uses banned builtin `open`" |
| `sorted(xs, key=eval("..."))` | not caught by the pre-filter, ran in the sandbox | refused: "line 1 uses banned builtin `eval`" |
| terminal closed under a finished dashboard | aco kept running at 100% CPU (found as two orphaned processes, 65+ minutes) | exits 130 in about 0.06 s |
| local imports, packages | project/main.py: `ModuleNotFoundError: No module named 'helpers'`; numpy_project: `No module named 'more_itertools'` | project closure copied read-only (21.5x confirmed with --mutate); wheels installed and cached (44.1x confirmed) |

## Sandbox isolation (runner-level, release build)

The CLI's policy pre-filter refuses `os`, `socket`, `open` and friends before anything runs (rows above), so isolation is tested one level down, by running hostile snippets straight through the runner. `cargo test --release --test sandbox --test abort -- --test-threads 1` on this box (re-run for this pass: 14/14 pass):

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

Run 41429944030 at commit 795848e was green: the stress-fixture reproducibility check, `cargo fmt --check`, clippy with `-D warnings`, and `cargo test -- --nocapture` (so skips are visible in the log). Docker 28.0.4 was available. All Docker isolation tests, the real-runner search tests, the 14 stress tests (including numpy through a derived `aco-deps` image) and the 2 hang-up tests **ran and passed** under Docker. The unshare tests **skipped**, as expected: `kernel.apparmor_restrict_unprivileged_userns = 1` on Ubuntu 24.04 makes `unshare` fail with `write failed /proc/self/uid_map: Operation not permitted`. aco reports that and refuses to run, rather than falling back to running unsandboxed.

The CI end-to-end steps ran the debug binary with `--sandbox docker --reps 3` (CPython 3.12.16 in `python:3.12-slim`):

- lookup.py: baseline 684.3 ms, candidate 2.717 ms, **251.8x, confirmed**; slower.py: both rewrites rejected, original kept
- project main.py with `--mutate shop/inventory.py` (6 files): **24.5x, confirmed**
- numpy_project/stats.py (numpy 2.3.4, more-itertools 10.5.0): **68.9x, confirmed**; the rerun printed "using cached packages" and confirmed 64.3x
- large_3000.py (3,004 lines): **19.2x, confirmed**

## Search controls

The built-in rules are deterministic, so on real runs `--gens`/`--pop` are observable but `--patience` rarely gets a chance to fire: the search stops as soon as the rules have nothing new ("search exhausted"). Each rule offers one combined rewrite of all its sites plus per-site variants (at most 6), which is why the large files measure 7 candidates. `--patience`, `--gens` on a search that keeps improving, and `--pop` with 10 proposals per generation are covered with scripted proposers in `tests/search_fake.rs`.

## Notes

- The real-model lookup.py win is a program that computes the same count for that input by other means (`bisect` over `range(2000)` instead of testing multiples of 7). It is a correct result for the oracle (same stdout, stderr and exit code under 10 seeds) but not a general equivalent, which is why a confirmed winner that includes a model rewrite is now flagged for review in the summary and the report (`best.model_rewrite`).
- The mock-model rows use a local OpenAI-compatible mock server; the hash-order trap uses one too, because no rule proposes `list(set(...))`.
- `ACO_TEST_PANIC=1` is a test-only hook that panics in the search thread, to prove the terminal is restored on a crash (exit 101).
- Speedups depend on the interpreter. CI's Docker image runs CPython 3.12; this box runs 3.13.5.
- [bench.md](bench.md) and [transcript.txt](transcript.txt) were regenerated in this pass with `python3 scripts/bench.py` (fixtures plus 15 stress cases).
