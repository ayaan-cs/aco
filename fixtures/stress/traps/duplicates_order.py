# Duplicates and order: membership does not depend on either, so the set
# rewrite is safe here and must give identical output.
order = [5, 3, 5, 1, 3, 3, 9, 1] * 400
picked = []
for v in range(12000):
    if v % 13 in order:
        picked.append(v % 13)
print(len(picked), picked[:10])
