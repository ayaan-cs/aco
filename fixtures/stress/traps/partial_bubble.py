# Three passes of bubble sort, not a full sort: replacing it with sorted()
# would change the output.
xs = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0] * 300
n = len(xs)
for i in range(3):
    for j in range(n - 1 - i):
        if xs[j] > xs[j + 1]:
            xs[j], xs[j + 1] = xs[j + 1], xs[j]
print(xs[:12], xs[-5:])
