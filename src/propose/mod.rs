//! Proposers turn a source file into candidate rewrites.

use serde::Serialize;

/// One proposed replacement for the whole file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// Full replacement source.
    pub source: String,
    /// Where it came from, for example `rule:bubble-sort` or `model:qwen2.5-coder:1.5b`.
    pub origin: String,
    /// Proposal tokens spent. Rules cost 0.
    pub tokens: u32,
}

/// Anything that can propose rewrites. Proposers never execute code: every
/// candidate goes through the policy and a [`crate::runner::Runner`].
pub trait Proposer: Send + Sync {
    fn name(&self) -> String;
    fn propose(&self, source: &str) -> Vec<Candidate>;
    /// Human-readable notes from the last `propose` call (dropped responses, errors).
    fn take_notes(&self) -> Vec<String> {
        Vec::new()
    }
}
