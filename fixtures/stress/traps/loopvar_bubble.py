# A full bubble sort, but the loop variables are read afterwards.
xs = [(i * 7919) % 1000 for i in range(1500)]
for i in range(len(xs)):
    for j in range(len(xs) - 1 - i):
        if xs[j] > xs[j + 1]:
            xs[j], xs[j + 1] = xs[j + 1], xs[j]
print(xs[0], xs[-1], i, j)
