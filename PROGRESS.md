# aco build progress

- [x] 1. Scaffold (crate, release profile, Cargo.lock, fixtures, Runner and Proposer traits)
- [ ] 2. Policy pre-filter and rule mutators
- [ ] 3. Scoring and search (in-sandbox timing harness, median, 5% gate, confirmation re-run, early stop, bounded pool)
- [ ] 4. Sandbox (Docker runner, unshare fallback, isolation tests)
- [ ] 5. Local model proposer (OpenAI-compatible HTTP, mock-server test)
- [ ] 6. Interface (Ratatui TUI, --plain, --json)
- [ ] 7. Docs and CI (LICENSE, README, SECURITY, CONTRIBUTING, ci.yml)
- [ ] 9. Release run on fixtures, transcript and bench in release-run/
- Not in this run: step 8 (portfolio sheet), pushing to GitHub.

## Notes
- Box: Debian 13, Python 3.13 at /usr/bin/python3, no Docker, unprivileged user namespaces work.
- Toolchain: rustup stable (cargo 1.99.1) with clippy at ~/.cargo/bin.
