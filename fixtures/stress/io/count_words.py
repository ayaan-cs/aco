# Reads stdin and takes a repeat count from argv; exits with status 3 when
# asked to, and reports to stderr.
import sys

times = int(sys.argv[1]) if len(sys.argv) > 1 else 1
stop = ["the", "a", "of", "and", "to", "in"]
text = sys.stdin.read()
kept = 0
for _ in range(times):
    for w in text.split():
        if w.lower() not in stop:
            kept += 1
print(kept)
print("processed", times, "rounds", file=sys.stderr)
if len(sys.argv) > 2 and sys.argv[2] == "fail":
    sys.exit(3)
