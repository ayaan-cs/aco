# Contributing

Thanks for helping. Run these before opening a pull request:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

## Rules for changes

- **A new mutator** (in `src/mutate.rs`) needs two tests: a speedup test that runs the real search on a fixture and asserts the rewrite is kept and confirmed, and a non-match test that shows a file without the pattern (or with a near miss that would change behaviour) is left unchanged. A rule should only emit source that passes `policy::check`.
- **A new proposer** implements `Proposer` and returns candidates only. It must not execute code. Every candidate goes through `policy::check` and a `Runner`; no proposer may spawn `python` or any other process on its own.
- **A runner change** needs a test that fails if the isolation it touches is removed. `tests/sandbox.rs` has the pattern: a host env var, a host file, a network connect, and a spin-loop timeout. Never weaken isolation to make a test pass. Skip the test with a clear reason instead.
- **Timing changes** must keep the timer inside the sandbox harness (`src/harness.py`), so that sandbox startup never enters a score.
- No `unsafe` code (the crate forbids it) and no async runtime.

## Showcase

The project is showcased on the author's portfolio page, so please don't build a second site here. If a demo page is added later, `--json` reports are the interface it should read.
