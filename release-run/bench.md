# aco release run: bench

Measured 2026-11-02 04:36 CST with the release build on this box (Python 3.13.5, 8 CPUs, unshare sandbox), `--plain --json`, rules proposer, default settings (5 reps after 1 warmup, 5% margin, 3 generations). Times are medians measured inside the sandbox by the harness; sandbox startup and package installs are not timed. Confirmation re-runs baseline and winner interleaved in fresh sandboxes, 10 rounds, with `PYTHONHASHSEED` 1-10. The full output is [transcript.txt](transcript.txt); numbers come from the `*.report.json` files ([bench.json](bench.json) has them all). Regenerate with `python3 scripts/bench.py`.

## Fixtures

| Fixture | Baseline (search) | Confirm: baseline | Confirm: winner | Speedup | Rounds won | Winner | Result |
|---|---|---|---|---|---|---|---|
| bubble.py | 317.624 ms | 319.881 ms | 778.3 µs | 411.0x | 10/10 | rule:bubble-sort:list.sort | confirmed |
| concat.py | 275.611 ms | 266.956 ms | 4.745 ms | 56.3x | 10/10 | rule:concat:join | confirmed |
| lookup.py | 244.494 ms | 246.096 ms | 1.345 ms | 183.0x | 10/10 | rule:membership:frozenset | confirmed |
| slower.py | 250.116 ms | - | - | - | - | (original kept) | no candidate matched the oracle and cleared the time margin |

## Stress and edge cases

| Case | Files / lines | Packages | Baseline | Winner median | Speedup | Rounds won | Result | Parse | Rules | Install | Search |
|---|---|---|---|---|---|---|---|---|---|---|---|
| large_300.py (300 lines) | 1 / 309 | - | 55.685 ms | 2.652 ms | 21.0x | 10/10 | confirmed | 0.6 ms | 15 ms | 0 ms | 3.1 s |
| large_1000.py (1,000 lines) | 1 / 1009 | - | 54.968 ms | 2.356 ms | 23.3x | 10/10 | confirmed | 2.3 ms | 44 ms | 0 ms | 3.3 s |
| large_3000.py (3,000 lines) | 1 / 3004 | - | 55.197 ms | 2.644 ms | 20.9x | 10/10 | confirmed | 5.0 ms | 132 ms | 0 ms | 3.9 s |
| project, 6 files, --mutate shop/inventory.py | 6 / 37 | - | 54.258 ms | 2.575 ms | 21.1x | 10/10 | confirmed | 0.0 ms | 1 ms | 0 ms | 2.6 s |
| numpy_project (numpy 2.3.4 + more-itertools) | 1 / 14 | more-itertools 10.5.0, numpy 2.3.4 | 195.470 ms | 4.316 ms | 45.3x | 10/10 | confirmed | 0.0 ms | 1 ms | 11 ms | 7.8 s |
| stdlib_heavy.py | 1 / 41 | - | 34.836 ms | - | - | - | no candidate matched the oracle and cleared the time margin | 0.1 ms | 1 ms | 0 ms | 0.5 s |
| traps/duplicates_order.py | 1 / 8 | - | 88.909 ms | 585.8 µs | 151.8x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 3.3 s |
| traps/set_output.py (prints a set) | 1 / 10 | - | 11.819 ms | 6.980 ms | 1.7x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 1.4 s |
| traps/alias_set.py (unsound to rewrite) | 1 / 10 | - | 4.393 ms | - | - | - | no candidate matched the oracle and cleared the time margin | 0.0 ms | 0 ms | 0 ms | 0.1 s |
| io/count_words.py --stdin input.txt -- 40 | 1 / 16 | - | 24.656 ms | 22.099 ms | 1.1x | 10/10 | confirmed | 0.0 ms | 1 ms | 0 ms | 2.1 s |
| io/seeded_random.py | 1 / 10 | - | 103.896 ms | 555.3 µs | 187.1x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 3.7 s |
| encoding/crlf.py (CRLF kept) | 1 / 8 | - | 148.992 ms | 131.5 µs | 1132.9x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 4.8 s |
| encoding/bom.py (BOM kept) | 1 / 8 | - | 153.352 ms | 141.7 µs | 1082.1x | 10/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 4.9 s |
| syntax/match_stmt.py | 1 / 16 | - | 2.568 ms | 1.751 ms | 1.5x | 8/10 | confirmed | 0.0 ms | 0 ms | 0 ms | 1.1 s |
| tiny.py (2 µs) | 1 / 7 | - | 2.0 µs | - | - | - | too fast to measure | 0.0 ms | 0 ms | 0 ms | 0.0 s |

Install is the package step (cached here after the first run; a cold numpy install measured 1.0 s in release-run/VERIFY.md). Rules is the time the rewrite rules took to analyse the file. Baseline is the confirmation baseline when a confirmation ran, else the search-phase median.
