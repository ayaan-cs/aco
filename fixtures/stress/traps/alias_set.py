# `seen` is mutated through an alias inside the loop, so membership in a
# set built before the loop would be stale. The rule must not fire.
seen = []
alias = seen
out = 0
for i in range(3000):
    if i % 50 in seen:
        out += 1
    alias.append(i % 37)
print(out)
