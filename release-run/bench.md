# aco release run: bench

Measured 2026-11-02 04:52 CST with the release build on this box (Python 3.13.5, 8 CPUs, unshare sandbox), `--plain --json`, rules proposer, default settings (5 reps after 1 warmup, 5% margin, 3 generations). Times are medians measured inside the sandbox by the harness; sandbox startup and package installs are not timed. Confirmation re-runs baseline and winner interleaved in fresh sandboxes, 10 rounds, with `PYTHONHASHSEED` 1-10. The full output is [transcript.txt](transcript.txt); numbers come from the `*.report.json` files ([bench.json](bench.json) has them all). Regenerate with `python3 scripts/bench.py`.

## Fixtures

| Fixture | Baseline (search) | Confirm: baseline | Confirm: winner | Speedup | Rounds won | Winner | Result |
|---|---|---|---|---|---|---|---|
| bubble.py | 317.895 ms | 319.321 ms | 787.1 µs | 405.7x | 10/10 | rule:bubble-sort:list.sort | confirmed |
| concat.py | 263.502 ms | 268.279 ms | 4.655 ms | 57.6x | 10/10 | rule:concat:join | confirmed |
| lookup.py | 241.493 ms | 244.206 ms | 1.371 ms | 178.1x | 10/10 | rule:membership:frozenset | confirmed |
| slower.py | 241.562 ms | - | - | - | - | (original kept) | no candidate matched the oracle and cleared the time margin |

## Stress and edge cases

| Case | Files / lines | Packages | Baseline | Winner median | Speedup | Rounds won | Result | Parse | Rules | Install | Search |
|---|---|---|---|---|---|---|---|---|---|---|---|
| large_300.py (300 lines) | 1 / 309 | - | 54.996 ms | 2.572 ms | 21.4x | 10/10 | confirmed | 0.5 ms | 15 ms | 0 ms | 3.1 s |
| large_1000.py (1,000 lines) | 1 / 1009 | - | 54.758 ms | 2.338 ms | 23.4x | 10/10 | confirmed | 3.1 ms | 45 ms | 0 ms | 3.2 s |
| large_3000.py (3,000 lines) | 1 / 3004 | - | 55.164 ms | 2.988 ms | 18.5x | 10/10 | confirmed | 5.0 ms | 133 ms | 0 ms | 3.7 s |
| project, 6 files, --mutate shop/inventory.py | 6 / 37 | - | 53.006 ms | 2.502 ms | 21.2x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 2.4 s |
| numpy_project (numpy 2.3.4 + more-itertools) | 1 / 14 | more-itertools 10.5.0, numpy 2.3.4 | 193.845 ms | 4.303 ms | 45.1x | 10/10 | confirmed | 0.0 ms | 1 ms | 11 ms | 7.7 s |
| stdlib_heavy.py | 1 / 41 | - | 35.037 ms | - | - | - | no candidate matched the oracle and cleared the time margin | 0.1 ms | 1 ms | 0 ms | 0.5 s |
| traps/duplicates_order.py | 1 / 8 | - | 89.448 ms | 599.0 µs | 149.3x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 3.2 s |
| traps/set_output.py (prints a set) | 1 / 10 | - | 11.772 ms | 6.904 ms | 1.7x | 9/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 1.5 s |
| traps/alias_set.py (unsound to rewrite) | 1 / 10 | - | 4.363 ms | - | - | - | no candidate matched the oracle and cleared the time margin | 0.0 ms | 0 ms | 0 ms | 0.1 s |
| io/count_words.py --stdin input.txt -- 40 | 1 / 16 | - | 24.586 ms | 21.610 ms | 1.1x | 10/10 | confirmed | 0.0 ms | 1 ms | 0 ms | 2.2 s |
| io/seeded_random.py | 1 / 10 | - | 104.959 ms | 498.1 µs | 210.7x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 3.6 s |
| encoding/crlf.py (CRLF kept) | 1 / 8 | - | 140.520 ms | 121.1 µs | 1160.5x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 4.5 s |
| encoding/bom.py (BOM kept) | 1 / 8 | - | 141.256 ms | 122.0 µs | 1158.1x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 4.5 s |
| syntax/match_stmt.py | 1 / 16 | - | 2.359 ms | 1.701 ms | 1.4x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 0.9 s |
| tiny.py (2 µs) | 1 / 7 | - | 1.9 µs | - | - | - | too fast to measure | 0.0 ms | 0 ms | 0 ms | 0.0 s |

Install is the package step (cached here after the first run; a cold numpy install measured 1.0 s in release-run/VERIFY.md). Rules is the time the rewrite rules took to analyse the file. Baseline is the confirmation baseline when a confirmation ran, else the search-phase median.
