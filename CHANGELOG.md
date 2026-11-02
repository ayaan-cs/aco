# Changelog

All notable changes to aco are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and aco uses [semantic versioning](https://semver.org/).

## [0.1.0] - 2026-11-02

First release.

### Search
- Evolutionary search for faster versions of a Python script: propose rewrites, run each in a sandbox, keep one only when its behaviour matches the original and its median time is at least 5% (`--margin`) lower; `--gens`, `--pop`, `--patience`, `--reps`, `--jobs`.
- Output check on stdout, stderr and exit code (or `--check FILE` for an exact expected stdout). Search runs use `PYTHONHASHSEED=0`.
- Confirmation re-run: baseline and winner interleaved in fresh sandboxes under `PYTHONHASHSEED` 1..N; the winner must behave identically under every seed, clear the margin and win at least 75% of the paired rounds.
- Scripts faster than 1 ms per run are reported as too fast to measure and left alone.
- Deterministic AST rewrite rules (bubble sort to `sorted`/`list.sort`, string `+=` in loops to `"".join`, list membership to a set built once), scope-aware and refusing to fire when the rewrite could change behaviour (aliasing, `global`, nested scopes, partial sorts, loop variables used afterwards).
- Optional local model proposer for any OpenAI-compatible server (Ollama, llama.cpp): `--proposer ollama|both`, `--endpoint`, `--model`, `--token-budget`. A confirmed winner that includes a model rewrite is flagged for review.

### Projects, packages and inputs
- Local imports: the script's import closure (packages, relative imports, namespace packages) is copied read-only into the sandbox; `--project-root`; `--mutate FILE` rewrites an imported module instead of the script; every local file passes the pre-filter, with `file:line` diagnostics.
- Third-party packages from `--requirements`, `requirements.txt` or `pyproject.toml`: wheels only, installed once outside the timed runs and cached (host site-packages for unshare, a derived `aco-deps` image for Docker); clear errors for packages without wheels and for imports missing from the requirements.
- `--stdin FILE` and script arguments after `--`; a small `sys` subset (argv, std streams, exit, limits).
- UTF-8 sources with or without BOM, LF or CRLF (both kept in the output); clear errors for other encodings and for unsupported newer syntax.

### Sandboxes
- Docker (`python:3.12-slim` pinned by digest): no network, read-only root, all capabilities dropped, `nobody`, pid/memory/CPU limits.
- unshare fallback on Linux: user, mount, PID and network namespaces, a private root with read-only `/usr`, no capabilities, cleared environment, `prlimit` limits. Fails closed when user namespaces are unavailable.
- No sandbox, no run: aco exits 3 and names what is missing.
- On macOS, per-run scratch directories live under `~/.cache/aco/run`, because colima and Lima share only the home directory with the Docker VM (`$TMPDIR` is under `/var/folders`). `ACO_SCRATCH_DIR` overrides the location.

### Interface
- Live Ratatui dashboard (progress, sparklines, candidate table, summary and winner diff), `--plain` line output, `--json` reports, colour honouring `NO_COLOR`, exit codes 0/1/2/3/130, signal-safe cancel that kills running sandboxes, and a clean exit when the terminal is closed.

### Distribution
- Prebuilt binaries for Linux (x86_64, aarch64; static musl) and macOS (x86_64, arm64) with SHA256SUMS, an install script, and `cargo install --git`. Minimum Rust version 1.88.

### Known limitations
- One file is rewritten per run; installed packages are not rewritten or policy-checked (the sandbox contains them).
- The output check covers the run's input, not every input.
- PEP 701 f-strings that reuse the outer quote inside a replacement field cannot be parsed yet.
- Docker on macOS is tested end to end in CI only on Intel (`macos-15-intel` with colima: the Docker isolation tests and a real `fixtures/bubble.py` run). On Apple silicon, CI builds and runs the test suite without a sandbox and checks that aco refuses to run (exit 3). Docker Desktop and OrbStack are not tested.
- Windows is supported only through WSL2.

[0.1.0]: https://github.com/ayaan-cs/aco/releases/tag/v0.1.0
