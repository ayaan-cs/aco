# A tight loop where hoisting a set looks clever but costs more than it saves:
# the list is rebuilt every row and only probed three times.

hits = 0
for row in range(1200000):
    small = [row % 3, 5, 9]
    for v in (1, 2, 3):
        if v in small:
            hits += 1

print(hits)
