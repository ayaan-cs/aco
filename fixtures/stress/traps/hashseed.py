# Order-preserving de-duplication done slowly. The words are listed in the
# order a set happens to iterate them under PYTHONHASHSEED=0, so a rewrite
# to list(set(words)) matches the output at seed 0 but not at most others.
words = ["alpha", "golf", "bravo", "echo", "kilo", "delta"] * 60000
seen = []
for w in words:
    if w not in seen:
        seen.append(w)
print(seen)
