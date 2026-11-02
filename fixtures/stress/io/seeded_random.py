import random

random.seed(42)
xs = [random.randint(0, 9999) for _ in range(1500)]
n = len(xs)
for i in range(n):
    for j in range(n - 1 - i):
        if xs[j] > xs[j + 1]:
            xs[j], xs[j + 1] = xs[j + 1], xs[j]
print(xs[:5], xs[-5:])
