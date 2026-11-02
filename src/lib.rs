//! aco: an autonomous code optimizer for Python.
//!
//! The search is evolutionary with a measured reward. It is not a trained
//! policy. Every candidate is parsed, pre-filtered by [`policy`], executed in
//! a sandbox by a [`runner::Runner`], and only kept when its output matches
//! the oracle and its in-sandbox median is clearly faster.

pub mod mutate;
pub mod policy;
pub mod propose;
pub mod runner;
pub mod score;
pub mod search;
