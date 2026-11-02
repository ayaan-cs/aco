//! End-to-end search with the rule proposer and the real sandbox runner.

mod common;

use aco::propose::RuleProposer;
use aco::runner::{select, Limits, Runner, SandboxKind};
use aco::score::Decision;
use aco::search::{optimize, Config, Report};
use common::no_cancel;

fn runner() -> Option<Box<dyn Runner>> {
    match select(SandboxKind::Auto, Limits::default()) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("SKIP real-runner search test: {e}");
            None
        }
    }
}

fn run(src: &str) -> Option<Report> {
    let r = runner()?;
    let cfg = Config {
        reps: 3,
        ..Config::default()
    };
    Some(
        optimize(
            "fixture",
            src,
            &RuleProposer,
            r.as_ref(),
            &cfg,
            None,
            &no_cancel(),
            &mut |_| {},
        )
        .unwrap(),
    )
}

fn assert_kept_and_confirmed(r: &Report, origin_prefix: &str) {
    assert!(r.best.confirmed, "{:#?}", r.best);
    assert!(
        r.best.origin.starts_with(origin_prefix),
        "{}",
        r.best.origin
    );
    assert!(r.best.speedup > 1.05);
    let c = r.confirmation.as_ref().unwrap();
    assert!(c.passed && c.oracle_ok);
    // Big enough that timer noise cannot decide the result. The absolute size
    // depends on the interpreter: concat.py measured ~258 ms on a Debian box's
    // CPython 3.13 but ~47 ms in CI's Docker image (CPython 3.12).
    assert!(
        r.baseline.median_ns >= 20_000_000,
        "fixture too small to time reliably: {} ns",
        r.baseline.median_ns
    );
}

#[test]
fn bubble_is_kept_and_confirmed() {
    if let Some(r) = run(include_str!("../fixtures/bubble.py")) {
        assert_kept_and_confirmed(&r, "rule:bubble-sort");
    }
}

#[test]
fn concat_is_kept_and_confirmed() {
    if let Some(r) = run(include_str!("../fixtures/concat.py")) {
        assert_kept_and_confirmed(&r, "rule:concat");
    }
}

#[test]
fn lookup_is_kept_and_confirmed() {
    if let Some(r) = run(include_str!("../fixtures/lookup.py")) {
        assert_kept_and_confirmed(&r, "rule:membership");
    }
}

#[test]
fn slower_edit_is_rejected() {
    if let Some(r) = run(include_str!("../fixtures/slower.py")) {
        assert!(!r.candidates.is_empty(), "the set rule should have fired");
        assert!(
            r.candidates
                .iter()
                .all(|c| c.decision == Decision::RejectMargin),
            "{:#?}",
            r.candidates
        );
        assert_eq!(r.best.origin, "baseline");
        assert!(r.confirmation.is_none());
        assert!(r.stop_reason.starts_with("early stop"));
    }
}
