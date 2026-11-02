# Building a large string with += in a loop (quadratic at module scope).

report = ""
for i in range(40000):
    report += str(i * 31 % 9973) + ","

print(len(report), report[:24], report[-24:])
