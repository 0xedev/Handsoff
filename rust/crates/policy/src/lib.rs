//! Policy engine: declarative rules for when to fail over.
//!
//! This is the "usage tracking → switch decision" half of the v0.4 split.
//! Snapshot quality lives in `handoff-context`; this crate decides *when*
//! and *to what*.

use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub failover: FailoverPolicy,
    #[serde(default)]
    pub critic: CriticPolicy,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FailoverPolicy {
    #[serde(default = "default_pct")]
    pub tokens_remaining_pct: f64,
    #[serde(default)]
    pub tokens_remaining_abs: Option<i64>,
    #[serde(default = "default_req")]
    pub requests_remaining: i64,
    #[serde(default = "default_chain")]
    pub chain: Vec<String>,
    #[serde(default = "yes")]
    pub auto_spawn: bool,
    #[serde(default)]
    pub summarize: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CriticPolicy {
    /// Local agent that drives the worker role (e.g. "claude", "codex").
    /// Aliased to `worker_model` for back-compat with v0.4.0-alpha configs.
    #[serde(default = "default_worker", alias = "worker_model")]
    pub worker_agent: String,
    /// Local agent that drives the critic role.
    #[serde(default = "default_critic", alias = "critic_model")]
    pub critic_agent: String,
    /// Local agent used for failover-snapshot summarisation.
    #[serde(default = "default_critic", alias = "summarizer_model")]
    pub summarizer_agent: String,
}

fn default_pct() -> f64 {
    10.0
}
fn default_req() -> i64 {
    5
}
fn default_chain() -> Vec<String> {
    vec!["claude".into(), "codex".into(), "copilot".into()]
}
fn yes() -> bool {
    true
}
fn default_worker() -> String {
    "claude".into()
}
fn default_critic() -> String {
    "claude".into()
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            failover: FailoverPolicy::default(),
            critic: CriticPolicy::default(),
        }
    }
}

impl Default for FailoverPolicy {
    fn default() -> Self {
        Self {
            tokens_remaining_pct: default_pct(),
            tokens_remaining_abs: None,
            requests_remaining: default_req(),
            chain: default_chain(),
            auto_spawn: true,
            summarize: false,
        }
    }
}

impl Default for CriticPolicy {
    fn default() -> Self {
        Self {
            worker_agent: default_worker(),
            critic_agent: default_critic(),
            summarizer_agent: default_critic(),
        }
    }
}

pub fn load(project_root: &Path) -> Result<Policy> {
    let p = project_root.join(".handoff").join("config.toml");
    if !p.exists() {
        return Ok(Policy::default());
    }
    let body = std::fs::read_to_string(&p)?;
    let policy: Policy = toml::from_str(&body)?;
    Ok(policy)
}

#[derive(Debug, Clone)]
pub struct Trigger {
    pub fired: bool,
    pub reason: String,
}

/// Decide whether the current usage state should trigger a handoff.
pub fn should_trigger(
    p: &FailoverPolicy,
    tokens_remaining: Option<i64>,
    requests_remaining: Option<i64>,
    initial_tokens: Option<i64>,
) -> Trigger {
    if let (Some(tok), Some(abs)) = (tokens_remaining, p.tokens_remaining_abs) {
        if tok < abs {
            return Trigger {
                fired: true,
                reason: format!("tokens_remaining={tok} < abs={abs}"),
            };
        }
    }
    if let (Some(tok), Some(init)) = (tokens_remaining, initial_tokens) {
        if init > 0 {
            let pct = 100.0 * (tok as f64) / (init as f64);
            if pct < p.tokens_remaining_pct {
                return Trigger {
                    fired: true,
                    reason: format!(
                        "tokens_remaining={tok} ({:.1}% < {:.1}%)",
                        pct, p.tokens_remaining_pct
                    ),
                };
            }
        }
    }
    if let Some(req) = requests_remaining {
        if req < p.requests_remaining {
            return Trigger {
                fired: true,
                reason: format!("requests_remaining={req} < {}", p.requests_remaining),
            };
        }
    }
    Trigger {
        fired: false,
        reason: String::new(),
    }
}

/// Pick the next agent in the chain, skipping the current one.
pub fn pick_next<'a>(chain: &'a [String], current: Option<&str>) -> Option<&'a str> {
    chain
        .iter()
        .map(|s| s.as_str())
        .find(|k| Some(*k) != current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_below_pct() {
        let p = FailoverPolicy {
            tokens_remaining_pct: 10.0,
            ..Default::default()
        };
        let t = should_trigger(&p, Some(900), None, Some(10000));
        assert!(t.fired);
        assert!(t.reason.contains("tokens_remaining=900"));
    }

    #[test]
    fn no_trigger_above_pct() {
        let p = FailoverPolicy::default();
        let t = should_trigger(&p, Some(2000), None, Some(10000));
        assert!(!t.fired);
    }

    #[test]
    fn triggers_below_abs() {
        let p = FailoverPolicy {
            tokens_remaining_abs: Some(500),
            ..Default::default()
        };
        let t = should_trigger(&p, Some(300), None, None);
        assert!(t.fired);
        assert!(t.reason.contains("abs=500"));
    }

    #[test]
    fn pick_next_skips_current() {
        let chain = vec!["claude".to_string(), "codex".into(), "copilot".into()];
        assert_eq!(pick_next(&chain, Some("claude")), Some("codex"));
    }

    #[test]
    fn pick_next_first_when_unknown() {
        let chain = vec!["claude".to_string(), "codex".into()];
        assert_eq!(pick_next(&chain, Some("antigravity")), Some("claude"));
    }
}
