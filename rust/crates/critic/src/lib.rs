//! Cheap-worker / expensive-critic loop.
//!
//! v0.4-alpha note: the Rust port keeps the protocol but defers the
//! Anthropic-API client to a v0.4-beta. The Python implementation in
//! `../../src/handoff/critic/runner.py` is still callable and uses the
//! same artifact format.
//!
//! This crate currently exposes the prompt templates and result types so
//! other crates (notably `handoff-context`'s snapshot summarizer) can
//! depend on a stable surface that the eventual Rust client will fulfill.

use serde::{Deserialize, Serialize};

pub const WORKER_SYSTEM: &str = include_str!("worker_system.txt");
pub const CRITIC_SYSTEM: &str = include_str!("critic_system.txt");
pub const SUMMARIZER_SYSTEM: &str = include_str!("summarizer_system.txt");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticResult {
    pub verdict: String,
    pub plan: String,
    pub diff: String,
    pub notes: String,
    pub worker_tokens: u64,
    pub critic_tokens: u64,
    pub artifacts: Vec<String>,
}
