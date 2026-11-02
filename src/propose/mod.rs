//! Proposers turn a source file into candidate rewrites.

pub mod http;

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

/// The default proposer: deterministic AST rules with zero token cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuleProposer;

impl Proposer for RuleProposer {
    fn name(&self) -> String {
        "rules".into()
    }

    fn propose(&self, source: &str) -> Vec<Candidate> {
        crate::mutate::rewrites(source)
            .into_iter()
            .map(|r| Candidate {
                source: r.source,
                origin: format!("rule:{}", r.rule),
                tokens: 0,
            })
            .collect()
    }
}

/// Several proposers, queried in order.
pub struct Combined(pub Vec<Box<dyn Proposer>>);

impl Proposer for Combined {
    fn name(&self) -> String {
        self.0.iter().map(|p| p.name()).collect::<Vec<_>>().join("+")
    }

    fn propose(&self, source: &str) -> Vec<Candidate> {
        self.0.iter().flat_map(|p| p.propose(source)).collect()
    }

    fn take_notes(&self) -> Vec<String> {
        self.0.iter().flat_map(|p| p.take_notes()).collect()
    }
}
