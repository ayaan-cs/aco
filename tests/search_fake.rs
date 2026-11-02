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
    // Generation 1 finds it; generation 2 has nothing new for the deterministic
    // rules, so the search is exhausted before gen 3.
    assert_eq!(r.generations.len(), 2);
    assert_eq!(r.generations[1].evaluated, 0);
    assert!(
        r.stop_reason.starts_with("search exhausted"),
        "{}",
        r.stop_reason
    );
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

/// Proposes `width` new variants per call: one that adds a `# step` line (the
/// [`Halving`] runner makes each step twice as fast) and others that only add
/// a unique comment. Never deterministic, so only --gens and --patience stop it.
struct Stepper {
    width: usize,
    improve: bool,
    calls: std::sync::atomic::AtomicUsize,
}

impl Stepper {
    fn new(width: usize, improve: bool) -> Self {
        Stepper {
            width,
            improve,
            calls: Default::default(),
        }
    }
}

impl aco::propose::Proposer for Stepper {
    fn name(&self) -> String {
        "stepper".into()
    }
    fn propose(&self, source: &str) -> Vec<aco::propose::Candidate> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        (0..self.width)
            .map(|i| {
                let extra = if i == 0 && self.improve {
                    "# step\n".to_string()
                } else {
                    format!("# noop {call}-{i}\n")
                };
                aco::propose::Candidate {
                    source: format!("{source}{extra}"),
                    origin: format!("stepper:{call}-{i}"),
                    tokens: 0,
                }
            })
            .collect()
    }
}

/// 100 ms halved for every `# step` line.
struct Halving;

impl aco::runner::Runner for Halving {
    fn name(&self) -> &'static str {
        "halving"
    }
    fn run(&self, source: &str, reps: usize) -> aco::runner::Outcome {
        let steps = source.matches("# step").count() as u32;
        let ns = 100_000_000u64 >> steps.min(20);
        aco::runner::Outcome {
            stdout_sha256: "same".into(),
            times_ns: vec![ns; reps.max(1)],
            median_ns: ns,
            peak_rss_kb: 1,
            ..Default::default()
        }
    }
}

fn run_stepper(p: &Stepper, cfg: &Config) -> aco::search::Report {
    optimize(
        "s.py",
        "print(1)\n",
        p,
        &Halving,
        cfg,
        None,
        &no_cancel(),
        &mut |_| {},
    )
    .unwrap()
}

#[test]
fn no_improvement_stops_after_patience() {
    let cfg = Config {
        gens: 5,
        patience: 2,
        ..Config::default()
    };
    let r = run_stepper(&Stepper::new(3, false), &cfg);
    assert_eq!(r.generations.len(), 2);
    assert!(r.stop_reason.contains("--patience 2"), "{}", r.stop_reason);
    let cfg = Config {
        gens: 5,
        patience: 3,
        ..Config::default()
    };
    assert_eq!(
        run_stepper(&Stepper::new(3, false), &cfg).generations.len(),
        3
    );
}

#[test]
fn gens_caps_a_search_that_keeps_improving() {
    let cfg = Config {
        gens: 4,
        ..Config::default()
    };
    let r = run_stepper(&Stepper::new(3, true), &cfg);
    assert_eq!(r.generations.len(), 4);
    assert!(r.generations.iter().all(|g| g.new_elite));
    assert_eq!(r.stop_reason, "reached --gens 4");
    assert!(r.best.confirmed);
    assert_eq!(r.best.speedup, 16.0);
}

#[test]
fn pop_bounds_candidates_per_generation() {
    for pop in [2, 4, 7] {
        let cfg = Config {
            gens: 2,
            pop,
            ..Config::default()
        };
        let r = run_stepper(&Stepper::new(10, true), &cfg);
        assert!(
            r.generations.iter().all(|g| g.evaluated == pop - 1),
            "pop {pop}: {:?}",
            r.generations
                .iter()
                .map(|g| g.evaluated)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn rules_with_nothing_to_rewrite_stop_as_exhausted() {
    let runner = FakeRunner::new(100_000_000, vec![]);
    let r = optimize(
        "p.py",
        "print(sum(range(10)))\n",
        &RuleProposer,
        &runner,
        &Config {
            gens: 5,
            patience: 3,
            ..Config::default()
        },
        None,
        &no_cancel(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(r.generations.len(), 1);
    assert!(r.stop_reason.starts_with("search exhausted"));
    assert!(!r.best.confirmed);
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
