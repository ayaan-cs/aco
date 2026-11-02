# Uses time internally but prints only deterministic values.
import time

t0 = time.perf_counter()
words = ""
for i in range(30000):
    words += str(i % 10)
elapsed = time.perf_counter() - t0
print(len(words), elapsed >= 0)
