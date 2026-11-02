def make_items(n):
    seed = 3
    out = []
    for i in range(n):
        seed = (seed * 48271) % 2147483647
        out.append(("sku%d" % (seed % 5000), seed % 40))
    return out
