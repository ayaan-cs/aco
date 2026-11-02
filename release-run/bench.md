# aco release run: bench

Source: `transcript.txt` and the `*.report.json` files in this folder (release build, `--plain --json`, rules proposer, default reps 5, warmup 1, 5% margin). Times are medians measured inside the unshare sandbox by the harness; sandbox startup is not timed. Confirmation re-ran baseline and winner interleaved, 10 rounds each in fresh sandboxes.

| Fixture | Baseline median (search) | Confirm: baseline median | Confirm: kept median | Speedup | Kept variant | Result |
|---|---|---|---|---|---|---|
| bubble.py | 317.726 ms | 316.258 ms | 0.806 ms | 392.6x | rule:bubble-sort:list.sort | kept + confirmed |
| concat.py | 258.327 ms | 262.087 ms | 4.564 ms | 57.42x | rule:concat:join | kept + confirmed |
| lookup.py | 244.105 ms | 245.983 ms | 1.336 ms | 184.13x | rule:membership:frozenset | kept + confirmed |
| slower.py | 227.062 ms | - | - | - | (none, baseline kept) | rejected (reject_margin) |

slower.py: the set rule fired and was measured slower, so it was rejected and the baseline stays: rule:membership:set 297.8 ms (0.76x), rule:membership:frozenset 300.7 ms (0.75x) vs baseline 227.062 ms.

Machine: see the header of transcript.txt.
