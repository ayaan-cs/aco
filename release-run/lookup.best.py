# Repeated membership tests against a list inside a loop.

allowed = [i * 7 for i in range(2000)]
hits = 0
allowed_set = frozenset(allowed)
for k in range(35000):
    if k in allowed_set:
        hits += 1

print(hits)
