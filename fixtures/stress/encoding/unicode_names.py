# Non-ASCII identifiers and strings are fine.
café = ["é", "ü", "ß"] * 3
total = ""
for ch in range(20000):
    total += café[ch % 3]
print(len(total), total[:6])
