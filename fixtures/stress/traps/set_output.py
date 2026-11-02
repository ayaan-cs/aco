# Prints a set of strings, whose order depends on the hash seed. A correct
# rewrite elsewhere must still be accepted: baseline and candidate share
# the seed in every run.
words = ["w%d" % (i % 50) for i in range(60000)]
known = ["w%d" % i for i in range(0, 50, 2)]
hits = 0
for w in words:
    if w in known:
        hits += 1
print(hits, set(words[:10]))
