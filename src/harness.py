# aco timing harness. Runs inside the sandbox.
#
# argv: harness.py RUN_JSON
# RUN_JSON: {"target", "proj", "reps", "warmup", "nonce", "argv", "stdin", "deps"}
#
# The target runs as __main__ in a fresh module each rep, the way `python
# target.py` would: sys.path starts with the target's directory, then the
# project root and any installed requirements. Local project modules it
# imports are dropped from sys.modules after every rep, so their top-level code
# runs (and is timed) each time; the standard library and installed packages
# stay imported after the warmup.
#
# Prints exactly one line, "ACO-RESULT <nonce> <json>", on a private dup of the
# original stdout. The candidate's fd 1 points at /dev/null and its sys.stdout
# and sys.stderr are capture buffers, so it cannot forge the result line.
import builtins
import contextlib
import hashlib
import io
import json
import os
import resource
import sys
import time
import types

CAP = 1 << 20


def main():
    with open(sys.argv[1], "rb") as f:
        cfg = json.load(f)
    target, proj, nonce = cfg["target"], cfg["proj"], cfg["nonce"]
    result_fd = os.dup(1)
    devnull = os.open(os.devnull, os.O_WRONLY)
    os.dup2(devnull, 1)

    # `python -s` put the harness directory first; replace it with what a
    # plain `python target.py` run would have, plus the project root and deps.
    base = [p for p in sys.path[1:] if p]
    front = [os.path.dirname(target)]
    if proj not in front:
        front.append(proj)
    if cfg.get("deps"):
        base.append(cfg["deps"])
    sys.path[:] = front + base
    proj_prefix = proj.rstrip("/") + "/"

    result = {"times_ns": [], "error": None}
    outputs = []
    harness_main = sys.modules.get("__main__")
    preloaded = set(sys.modules)
    try:
        with open(target, "rb") as f:
            code = compile(f.read(), target, "exec")

        def local_modules():
            out = []
            for name, mod in list(sys.modules.items()):
                if name in preloaded:
                    continue
                path = getattr(mod, "__file__", None) or ""
                paths = list(getattr(mod, "__path__", None) or [])
                if path.startswith(proj_prefix) or any(
                    str(p).startswith(proj_prefix) for p in paths
                ):
                    out.append(name)
            return out

        stdin_bytes = (cfg.get("stdin") or "").encode("utf-8", "surrogateescape")

        def stream(data=b""):
            # A real text stream over bytes, so `.buffer`, `.readline()` and
            # iteration behave like a file, as they would under `python`.
            return io.TextIOWrapper(
                io.BytesIO(data), encoding="utf-8", errors="surrogateescape",
                write_through=True,
            )

        def once():
            out, err = stream(), stream()
            mod = types.ModuleType("__main__")
            mod.__file__ = target
            mod.__builtins__ = builtins
            sys.modules["__main__"] = mod
            sys.argv = [target] + list(cfg.get("argv") or [])
            sys.stdin = stream(stdin_bytes)
            exit_code = 0
            try:
                with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                    t0 = time.perf_counter_ns()
                    try:
                        exec(code, mod.__dict__)
                    except SystemExit as e:
                        c = e.code
                        if c is None:
                            exit_code = 0
                        elif isinstance(c, int):
                            exit_code = c
                        else:
                            print(c, file=sys.stderr)
                            exit_code = 1
                    t1 = time.perf_counter_ns()
                    out.flush()
                    err.flush()
            finally:
                sys.modules["__main__"] = harness_main
                for name in local_modules():
                    del sys.modules[name]
            return t1 - t0, (out.buffer.getvalue(), err.buffer.getvalue(), exit_code)

        for _ in range(cfg["warmup"]):
            once()
        for _ in range(cfg["reps"]):
            dt, obs = once()
            result["times_ns"].append(dt)
            outputs.append(obs)
    except BaseException as e:  # noqa: BLE001 - anything else is a failed run
        result["error"] = ("%s: %s" % (type(e).__name__, e))[:500]

    stdout, stderr, exit_code = outputs[0] if outputs else (b"", b"", 0)
    if result["error"] is None and len(set(outputs)) > 1:
        result["error"] = (
            "nondeterministic output: stdout, stderr or the exit code differed "
            "between repetitions of the same code (printing random numbers, times, "
            "ids or unordered data?). aco compares outputs to check a rewrite, so "
            "the output must be repeatable: seed `random`, or stop printing the "
            "varying values"
        )
    text = lambda b: b[:CAP].decode("utf-8", "replace")  # noqa: E731
    result["stdout"] = text(stdout)
    result["stdout_truncated"] = len(stdout) > CAP
    result["stdout_sha256"] = hashlib.sha256(stdout).hexdigest()
    result["stderr"] = text(stderr)
    result["stderr_sha256"] = hashlib.sha256(stderr).hexdigest()
    result["exit_code"] = exit_code
    result["maxrss_kb"] = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    try:
        with open("/sys/fs/cgroup/memory.peak") as f:
            result["cgroup_peak_bytes"] = int(f.read().strip())
    except (OSError, ValueError):
        result["cgroup_peak_bytes"] = None

    line = "ACO-RESULT %s %s\n" % (nonce, json.dumps(result))
    os.write(result_fd, line.encode())


main()
