# Standard-library heavy code: none of it may confuse import analysis or
# the rules, and the one slow pattern (string building) is still found.
import heapq
import itertools
import json
import re
from collections import Counter, defaultdict, deque
from dataclasses import dataclass, field
from functools import lru_cache


@dataclass(order=True)
class Job:
    priority: int
    name: str = field(compare=False)


@lru_cache(maxsize=None)
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)


def main():
    words = re.findall(r"[a-z]+", "the quick brown fox jumps over the lazy dog " * 200)
    counts = Counter(words)
    groups = defaultdict(list)
    for w in words[:400]:
        groups[len(w)].append(w)
    q = deque(range(10), maxlen=5)
    heap = [Job(p % 17, "job%d" % p) for p in range(300)]
    heapq.heapify(heap)
    top = [heapq.heappop(heap).name for _ in range(3)]
    pairs = list(itertools.islice(itertools.combinations(range(30), 2), 5))
    log = ""
    for i in range(40000):
        log += json.dumps([i % 7])
    print(counts.most_common(2), sorted(groups)[:3], list(q), top, pairs, fib(60))
    print(len(log))


main()
