#![allow(dead_code)]
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use aco::runner::{Outcome, Runner};
use aco::search::Event;

/// A runner that never executes anything: it maps source text to a fixed
/// timing and output, and records every source it was asked to run.
pub struct FakeRunner {
    pub rules: Vec<(&'static str, u64, &'static str)>,
    pub default_ns: u64,
    pub seen: Mutex<Vec<String>>,
}

impl FakeRunner {
    pub fn new(default_ns: u64, rules: Vec<(&'static str, u64, &'static str)>) -> Self {
        FakeRunner {
            rules,
            default_ns,
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl Runner for FakeRunner {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn run(&self, source: &str, reps: usize) -> Outcome {
        self.seen.lock().unwrap().push(source.to_string());
        let (ns, out) = self
            .rules
            .iter()
            .find(|(needle, _, _)| source.contains(needle))
            .map(|(_, ns, out)| (*ns, *out))
            .unwrap_or((self.default_ns, "same"));
        Outcome {
            stdout: out.into(),
            stdout_sha256: out.into(),
            times_ns: vec![ns; reps.max(1)],
            median_ns: ns,
            peak_rss_kb: 10_000,
            ..Default::default()
        }
    }
}

pub fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

pub fn sink(events: &mut Vec<Event>) -> impl FnMut(Event) + '_ {
    move |e| events.push(e)
}
