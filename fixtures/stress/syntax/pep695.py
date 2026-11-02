type Pair = tuple[int, int]


def first[T](xs: list[T]) -> T:
    return xs[0]


print(first([3, 1, 2]))
