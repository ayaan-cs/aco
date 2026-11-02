# aco timing harness. Runs inside the sandbox; the candidate never sees these
# imports because it executes in a fresh globals dict.
#
# argv: harness.py CANDIDATE REPS WARMUP NONCE
# Prints exactly one line, "ACO-RESULT <nonce> <json>", on a private dup of the
# original stdout. The candidate's own fd 1 points at /dev/null and its
# sys.stdout is a capture buffer, so it cannot forge the result line.
import builtins
import contextlib
import hashlib
import io
import json
import os
import resource
import sys
import time

STDOUT_CAP = 1 << 20


def main():
    path, reps, warmup, nonce = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
    result_fd = os.dup(1)
    devnull = os.open(os.devnull, os.O_WRONLY)
    os.dup2(devnull, 1)
    sys.argv = [path]

    result = {"times_ns": [], "error": None}
    outputs = []
    try:
        with open(path, "rb") as f:
            code = compile(f.read(), path, "exec")

        def once():
            buf = io.StringIO()
            env = {"__name__": "__main__", "__builtins__": builtins}
            with contextlib.redirect_stdout(buf):
                t0 = time.perf_counter_ns()
                exec(code, env)
                t1 = time.perf_counter_ns()
            return t1 - t0, buf.getvalue()

        for _ in range(warmup):
            once()
        for _ in range(reps):
            dt, out = once()
            result["times_ns"].append(dt)
            outputs.append(out)
    except BaseException as e:  # noqa: BLE001 - SystemExit and friends are rejects too
        result["error"] = ("%s: %s" % (type(e).__name__, e))[:500]

    first = outputs[0] if outputs else ""
    if result["error"] is None and len(set(outputs)) > 1:
        result["error"] = "nondeterministic stdout across reps"
    result["stdout"] = first[:STDOUT_CAP]
    result["stdout_truncated"] = len(first) > STDOUT_CAP
    result["stdout_sha256"] = hashlib.sha256(first.encode("utf-8", "surrogatepass")).hexdigest()
    result["maxrss_kb"] = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    try:
        with open("/sys/fs/cgroup/memory.peak") as f:
            result["cgroup_peak_bytes"] = int(f.read().strip())
    except (OSError, ValueError):
        result["cgroup_peak_bytes"] = None

    line = "ACO-RESULT %s %s\n" % (nonce, json.dumps(result))
    os.write(result_fd, line.encode())


main()
