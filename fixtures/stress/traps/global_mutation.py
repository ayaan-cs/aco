# A function called in the loop mutates the module-level list.
pool = [1, 2, 3]


def grow(v):
    pool.append(v)


hits = 0
for i in range(2000):
    if i in pool:
        hits += 1
    grow(i + 1)
print(hits)
