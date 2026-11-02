//! Runners execute a candidate inside a sandbox under the timing harness.

use serde::Serialize;

/// What one sandboxed execution produced.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Outcome {
    /// Captured stdout of the first timed rep.
    pub stdout: String,
    /// True when `stdout` was cut at the harness cap.
    pub stdout_truncated: bool,
    /// SHA-256 of the full stdout, computed inside the sandbox.
    pub stdout_sha256: String,
    /// Per-rep wall times in nanoseconds, measured inside the sandbox.
    pub times_ns: Vec<u64>,
    /// Median of `times_ns`.
    pub median_ns: u64,
    /// Peak resident set size in KiB, as reported by the sandbox.
    pub peak_rss_kb: u64,
    /// The wall-clock limit killed the run.
    pub timed_out: bool,
    /// Exception, sandbox failure, or harness error.
    pub error: Option<String>,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        !self.timed_out && self.error.is_none() && !self.times_ns.is_empty()
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        Outcome {
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}

/// Executes `source` with `reps` timed repetitions after the configured warmup.
pub trait Runner: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, source: &str, reps: usize) -> Outcome;
}
