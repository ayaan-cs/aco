# Building a large string with += in a loop (quadratic at module scope).

report_parts = []
for i in range(40000):
    report_parts.append(str(i * 31 % 9973) + ",")
report = "".join(report_parts)

print(len(report), report[:24], report[-24:])
