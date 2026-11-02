# Pure-Python bubble sort of a few thousand pseudo-random ints.


def bubble(xs):
    n = len(xs)
    xs.sort()
    return xs


seed = 12345
data = []
for _ in range(3500):
    seed = (seed * 1103515245 + 12345) % 2147483648
    data.append(seed % 100000)

out = bubble(data)
print(out[0], out[len(out) // 2], out[-1], sum(out[:100]))
