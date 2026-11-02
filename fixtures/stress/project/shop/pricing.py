from . import util


def total(items):
    return sum(util.clamp(q, 0, 25) for _, q in items)
