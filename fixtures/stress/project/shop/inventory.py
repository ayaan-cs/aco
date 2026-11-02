from .util import clamp


def restock_report(items):
    discontinued = ["sku%d" % i for i in range(0, 5000, 2)]
    restock = 0
    for sku, qty in items:
        if sku in discontinued:
            continue
        restock += clamp(30 - qty, 0, 30)
    return restock
