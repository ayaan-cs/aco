# 3.10 match statement and a walrus: parsed and left alone.
def kind(v):
    match v:
        case 0:
            return "zero"
        case [x, *_]:
            return "list"
        case _:
            return "other"


out = ""
for i in range(20000):
    if (k := kind(i % 2)) == "zero":
        out += k[0]
print(len(out))
