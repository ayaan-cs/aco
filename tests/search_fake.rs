mod common;

use aco::propose::RuleProposer;
use aco::score::Decision;
use aco::search::{optimize, Config, Event};
use common::{no_cancel, FakeRunner};

const BUBBLE: &str = include_str!("../fixtures/bubble.py");

#[test]
fn keeps_and_confirms_a_faster_variant_and_stops_early() {
    let runner = FakeRunner::new(100_000_000, vec![("sorted(xs)", 2_000_000, "same")]);
    let mut events = Vec::new();
    let cfg = Config::default();
    let r = optimize(
        "bubble.py",
        BUBBLE,
        &RuleProposer,
        &runner,
        &cfg,
        None,
        &no_cancel(),
        &mut |e| events.push(e),
    )
    .unwrap();
    assert!(r.best.confirmed);
    assert_eq!(r.best.origin, "rule:bubble-sort:sorted");
    // Generation 1 finds it; generation 2 has nothing new, so patience 1 stops before gen 3.
    assert_eq!(r.generations.len(), 2);
    assert!(r.stop_reason.starts_with("early stop"));
    let c = r.confirmation.unwrap();
    assert_eq!(c.rounds, 10);
    assert!(c.passed);
    // Candidate events come out in candidate order.
    let idx: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            Event::Candidate(c) => Some(c.index),
            _ => None,
        })
        .collect();
    assert_eq!(idx, (0..idx.len()).collect::<Vec<_>>());
}

#[test]
fn oracle_mismatch_is_rejected_however_fast() {
    let runner = FakeRunner::new(
        100_000_000,
        vec![
            ("sorted(xs)", 1_000, "different"),
            ("xs.sort()", 1_000, "different"),
        ],
    );
    let r = optimize(
        "b.py",
        BUBBLE,
        &RuleProposer,
        &runner,
        &Config::default(),
        None,
        &no_cancel(),
        &mut |_| {},
    )
    .unwrap();
    assert!(r
        .candidates
        .iter()
        .all(|c| c.decision == Decision::RejectOracle));
    assert_eq!(r.best.origin, "baseline");
    assert!(r.confirmation.is_none());
}

#[test]
fn a_lucky_median_that_does_not_repeat_fails_confirmation() {
    /// Fast during the search (reps > 1), slow when re-measured one rep at a time.
    struct Lucky;
    impl aco::runner::Runner for Lucky {
        fn name(&self) -> &'static str {
            "lucky"
        }
        fn run(&self, source: &str, reps: usize) -> aco::runner::Outcome {
            let ns = if source.contains("sorted(xs)") && reps > 1 {
                1_000_000
            } else {
                100_000_000
            };
            aco::runner::Outcome {
                stdout_sha256: "same".into(),
                times_ns: vec![ns; reps],
                median_ns: ns,
                peak_rss_kb: 1,
                ..Default::default()
            }
        }
    }
    let r = optimize(
        "b.py",
        BUBBLE,
        &RuleProposer,
        &Lucky,
        &Config::default(),
        None,
        &no_cancel(),
        &mut |_| {},
    )
    .unwrap();
    assert!(!r.best.confirmed);
    assert_eq!(r.best.origin, "baseline");
    assert!(r.best.reason.contains("failed confirmation"));
}

#[test]
fn no_improvement_stops_after_patience() {
    let runner = FakeRunner::new(100_000_000, vec![]);
    let src = "print(sum(range(10)))\n";
    let cfg = Config {
        gens: 5,
        patience: 2,
        ..Config::default()
    };
    let r = optimize(
        "p.py",
        src,
        &RuleProposer,
        &runner,
        &cfg,
        None,
        &no_cancel(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(r.generations.len(), 2);
    assert!(r.stop_reason.contains("--patience 2"));
}

#[test]
fn policy_violating_input_never_runs() {
    let runner = FakeRunner::new(1, vec![]);
    let err = optimize(
        "x.py",
        "import os\n",
        &RuleProposer,
        &runner,
        &Config::default(),
        None,
        &no_cancel(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(err.contains("banned module `os`"));
    assert!(runner.seen.lock().unwrap().is_empty());
}
