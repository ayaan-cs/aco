# Runs in microseconds: any speedup is inside the noise.
allowed = [2, 3, 5]
hits = 0
for v in range(8):
    if v in allowed:
        hits += 1
print(hits)
