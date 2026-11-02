import numpy as np
from more_itertools import chunked

rng = np.random.default_rng(1234)
m = rng.integers(0, 100, size=(200, 200))
col = (m @ m.T).sum(axis=0)
flagged = [int(v) % 1000 for v in col]
labels = list(range(0, 20000, 7))
hits = 0
for chunk in chunked(range(20000), 500):
    for v in chunk:
        if v in labels:
            hits += 1
print(int(col.max()), sum(flagged), hits)
