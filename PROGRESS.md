# aco build progress

- [x] 1. Scaffold (crate, release profile, Cargo.lock, fixtures, Runner and Proposer traits)
- [x] 2. Policy pre-filter and rule mutators
- [x] 3. Scoring and search (in-sandbox timing harness, median, 5% gate, confirmation re-run, early stop, bounded pool)
  - Real-runner search tests (keep+confirm bubble/concat/lookup, reject slower) land with step 4.
- [x] 4. Sandbox (Docker runner, unshare fallback, isolation tests)
  - unshare isolation tests pass on this box; Docker cases skip (no Docker installed). Removing --net or the env scrub makes them fail (checked).
- [x] 5. Local model proposer (OpenAI-compatible HTTP, mock-server test)
- [x] 6. Interface (Ratatui TUI, --plain, --json)
- [x] 7. Docs and CI (LICENSE, README, SECURITY, CONTRIBUTING, ci.yml)
- [x] 9. Release run on fixtures, transcript and bench in release-run/
- Not in this run: step 8 (portfolio sheet), pushing to GitHub.

## Notes
- Box: Debian 13, Python 3.13 at /usr/bin/python3, no Docker, unprivileged user namespaces work.
- Toolchain: rustup stable (cargo 1.99.1) with clippy at ~/.cargo/bin.

## Status (2026-11-02)
- cargo test: 41 passed (the 5 Docker isolation cases skip on this box: no Docker installed).
- cargo clippy --all-targets -- -D warnings: clean.
- Release run: release-run/transcript.txt, release-run/bench.md, release-run/bench.json, per-fixture *.report.json and *.best.py.
- Remaining for the parent: push to https://github.com/ayaan-cs/aco (needs a token), then step 8 (portfolio sheet) in the portfolio repo using release-run/bench.md.
- Unverified here: the Docker runner (CI on ubuntu-latest has Docker and will exercise it; unshare cases will likely skip there because of Ubuntu's AppArmor userns restriction).
