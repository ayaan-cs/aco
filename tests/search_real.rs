//! End-to-end search with the rule proposer and the real sandbox runner.

mod common;

use aco::propose::RuleProposer;
use aco::runner::{select, Backend, Bound, Limits, SandboxKind};
use aco::score::Decision;
use aco::search::{optimize, Config, Report};
use common::no_cancel;

fn runner() -> Option<Box<dyn Backend>> {
    match select(SandboxKind::Auto, Limits::default()) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("SKIP real-runner search test: {e}");
            None
        }
    }
}

fn run(src: &str) -> Option<Report> {
    let b = runner()?;
    let r = Bound::single(b.as_ref());
    let cfg = Config {
        reps: 3,
        ..Config::default()
    };
    Some(
        optimize(
            "fixture",
            src,
            &RuleProposer,
            &r,
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

/// Offers one fixed rewrite, like a model would.
struct Fixed(&'static str);

impl aco::propose::Proposer for Fixed {
    fn name(&self) -> String {
        "fixed".into()
    }
    fn propose(&self, _source: &str) -> Vec<aco::propose::Candidate> {
        vec![aco::propose::Candidate {
            source: self.0.to_string(),
            origin: "fixed".into(),
            tokens: 0,
        }]
    }
    fn deterministic(&self) -> bool {
        true
    }
}

fn run_with(src: &str, proposer: &dyn aco::propose::Proposer) -> Option<Report> {
    let b = runner()?;
    let r = Bound::single(b.as_ref());
    let cfg = Config {
        reps: 3,
        ..Config::default()
    };
    Some(
        optimize(
            "fixture",
            src,
            proposer,
            &r,
            &cfg,
            None,
            &no_cancel(),
            &mut |_| {},
        )
        .unwrap(),
    )
}

#[test]
fn hash_order_dependent_rewrite_is_caught_by_seeded_confirmation() {
    // Under PYTHONHASHSEED=0 (the search) list(set(words)) prints the same as
    // the order-preserving loop; under the confirmation's seeds 1..6 it does
    // not, so it must never be reported.
    let src = include_str!("../fixtures/stress/traps/hashseed.py");
    let wrong = "words = [\"alpha\", \"golf\", \"bravo\", \"echo\", \"kilo\", \"delta\"] * 60000\nprint(list(set(words)))\n";
    let Some(r) = run_with(src, &Fixed(wrong)) else {
        return;
    };
    assert!(!r.best.confirmed, "{:#?}", r.best);
    assert_eq!(r.best.origin, "baseline");
    match &r.confirmation {
        // Kept in the search, then caught with another seed.
        Some(c) => {
            eprintln!("hash-seed trap caught in confirmation: {:?}", c.detail);
            assert!(!c.oracle_ok);
            assert!(
                c.detail
                    .as_deref()
                    .unwrap_or("")
                    .contains("PYTHONHASHSEED="),
                "{c:#?}"
            );
        }
        // Or this interpreter orders the set differently at seed 0 too.
        None => assert_eq!(r.candidates[0].decision, Decision::RejectOracle),
    }

    // The correct order-preserving rewrite passes under every seed.
    let right = "words = [\"alpha\", \"golf\", \"bravo\", \"echo\", \"kilo\", \"delta\"] * 60000\nprint(list(dict.fromkeys(words)))\n";
    let r = run_with(src, &Fixed(right)).unwrap();
    assert!(r.best.confirmed, "{:#?}", r.best);
    let c = r.confirmation.unwrap();
    assert_eq!(c.seeds, (1..=6).collect::<Vec<u32>>());
}

#[test]
fn stderr_and_exit_code_are_part_of_the_output_check() {
    let src = "import sys\ntotal = 0\nfor i in range(300000):\n    total += i % 7\nprint(total)\nprint('warn', file=sys.stderr)\nsys.exit(2)\n";
    // Faster and same stdout, but silent on stderr.
    let quiet = "import sys\nprint(sum(i % 7 for i in range(300000)))\nsys.exit(2)\n";
    let Some(r) = run_with(src, &Fixed(quiet)) else {
        return;
    };
    assert_eq!(r.baseline.exit_code, 2);
    assert_eq!(r.candidates[0].decision, Decision::RejectOracle);
    assert_eq!(r.candidates[0].detail.as_deref(), Some("stderr differs"));
    // Same streams, different exit status.
    let exit0 =
        "import sys\nprint(sum(i % 7 for i in range(300000)))\nprint('warn', file=sys.stderr)\n";
    let r = run_with(src, &Fixed(exit0)).unwrap();
    assert_eq!(r.candidates[0].detail.as_deref(), Some("exit code differs"));
}
