//! Medians, the keep/reject gate, and the reward.

use serde::Serialize;

use crate::runner::Outcome;

/// Median of a sample. Even-length samples average the two middle values.
pub fn median(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let mid = v.len() / 2;
    if v.len() % 2 == 1 {
        v[mid]
    } else {
        ((v[mid - 1] as u128 + v[mid] as u128) / 2) as u64
    }
}

/// What a candidate's observable behaviour must equal.
#[derive(Clone, Debug)]
pub enum Oracle {
    /// The baseline's stdout and stderr (by hash) and exit code.
    Baseline {
        sha256: String,
        stderr_sha256: String,
        exit_code: i64,
    },
    /// A user-supplied `--check` file, compared exactly, plus the baseline's
    /// exit code.
    Expected { stdout: String, exit_code: i64 },
}

impl Oracle {
    /// The oracle a baseline run defines.
    pub fn of(baseline: &Outcome) -> Self {
        Oracle::Baseline {
            sha256: baseline.stdout_sha256.clone(),
            stderr_sha256: baseline.stderr_sha256.clone(),
            exit_code: baseline.exit_code,
        }
    }

    pub fn matches(&self, o: &Outcome) -> bool {
        self.mismatch(o).is_none()
    }

    /// Which observable differs, if any.
    pub fn mismatch(&self, o: &Outcome) -> Option<&'static str> {
        match self {
            Oracle::Baseline {
                sha256,
                stderr_sha256,
                exit_code,
            } => {
                if &o.stdout_sha256 != sha256 {
                    Some("stdout differs")
                } else if &o.stderr_sha256 != stderr_sha256 {
                    Some("stderr differs")
                } else if o.exit_code != *exit_code {
                    Some("exit code differs")
                } else {
                    None
                }
            }
            Oracle::Expected { stdout, exit_code } => {
                if o.stdout_truncated || &o.stdout != stdout {
                    Some("stdout differs from --check")
                } else if o.exit_code != *exit_code {
                    Some("exit code differs")
                } else {
                    None
                }
            }
        }
    }
}

/// True when two runs behaved the same (stdout, stderr, exit code).
pub fn same_behaviour(a: &Outcome, b: &Outcome) -> bool {
    a.stdout_sha256 == b.stdout_sha256
        && a.stderr_sha256 == b.stderr_sha256
        && a.exit_code == b.exit_code
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Keep,
    RejectPolicy,
    RejectTimeout,
    RejectError,
    RejectOracle,
    RejectMargin,
}

impl Decision {
    pub fn label(self) -> &'static str {
        match self {
            Decision::Keep => "KEEP",
            Decision::RejectPolicy => "reject (policy)",
            Decision::RejectTimeout => "reject (timeout)",
            Decision::RejectError => "reject (error)",
            Decision::RejectOracle => "reject (output differs)",
            Decision::RejectMargin => "reject (under margin)",
        }
    }
}

/// Time is the gate: the oracle must match and the median must be at least
/// `margin` under `reference_median_ns`.
pub fn judge(reference_median_ns: u64, oracle: &Oracle, cand: &Outcome, margin: f64) -> Decision {
    if cand.timed_out {
        Decision::RejectTimeout
    } else if !cand.ok() {
        Decision::RejectError
    } else if !oracle.matches(cand) {
        Decision::RejectOracle
    } else if !clears_margin(reference_median_ns, cand.median_ns, margin) {
        Decision::RejectMargin
    } else {
        Decision::Keep
    }
}

pub fn clears_margin(reference_ns: u64, candidate_ns: u64, margin: f64) -> bool {
    (candidate_ns as f64) <= (reference_ns as f64) * (1.0 - margin)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Reward {
    pub speedup: f64,
    pub memory_ratio: f64,
    pub token_ratio: f64,
    pub value: f64,
}

pub const MEMORY_WEIGHT: f64 = 0.15;
pub const TOKEN_WEIGHT: f64 = 0.05;

/// `speedup - 0.15 * memory_ratio - 0.05 * token_ratio`, higher is better.
/// Only used to rank candidates that already passed [`judge`].
pub fn reward(baseline: &Outcome, cand: &Outcome, tokens: u32, token_budget: u32) -> Reward {
    let speedup = baseline.median_ns as f64 / (cand.median_ns.max(1)) as f64;
    let memory_ratio = cand.peak_rss_kb as f64 / (baseline.peak_rss_kb.max(1)) as f64;
    let token_ratio = tokens as f64 / (token_budget.max(1)) as f64;
    Reward {
        speedup,
        memory_ratio,
        token_ratio,
        value: speedup - MEMORY_WEIGHT * memory_ratio - TOKEN_WEIGHT * token_ratio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(median_ns: u64, sha: &str) -> Outcome {
        Outcome {
            times_ns: vec![median_ns],
            median_ns,
            peak_rss_kb: 1000,
            stdout_sha256: sha.into(),
            ..Default::default()
        }
    }

    #[test]
    fn median_odd_and_even() {
        assert_eq!(median(&[5, 1, 3]), 3);
        assert_eq!(median(&[4, 1, 3, 2]), 2);
        assert_eq!(median(&[]), 0);
    }

    #[test]
    fn gate_requires_oracle_and_margin() {
        let o = Oracle::of(&out(1, "a"));
        assert_eq!(judge(100, &o, &out(95, "a"), 0.05), Decision::Keep);
        assert_eq!(judge(100, &o, &out(96, "a"), 0.05), Decision::RejectMargin);
        assert_eq!(judge(100, &o, &out(10, "b"), 0.05), Decision::RejectOracle);
        let mut t = out(10, "a");
        t.timed_out = true;
        assert_eq!(judge(100, &o, &t, 0.05), Decision::RejectTimeout);
        assert_eq!(
            judge(100, &o, &Outcome::failed("boom"), 0.05),
            Decision::RejectError
        );
        // stderr and the exit code are part of the behaviour.
        let mut e = out(10, "a");
        e.stderr_sha256 = "warn".into();
        assert_eq!(o.mismatch(&e), Some("stderr differs"));
        let mut x = out(10, "a");
        x.exit_code = 3;
        assert_eq!(judge(100, &o, &x, 0.05), Decision::RejectOracle);
    }

    #[test]
    fn reward_penalises_memory_and_tokens() {
        let b = out(100, "a");
        let r = reward(&b, &out(50, "a"), 256, 512);
        assert!((r.speedup - 2.0).abs() < 1e-9);
        assert!((r.value - (2.0 - 0.15 - 0.025)).abs() < 1e-9);
    }
}
