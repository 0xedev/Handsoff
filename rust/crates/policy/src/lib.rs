//! Policy engine: declarative rules for when to fail over.
//!
//! This is the "usage tracking → switch decision" half of the v0.4 split.
//! Snapshot quality lives in `handoff-context`; this crate decides *when*
//! and *to what*.

use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Policy {
    #[serde(default)]
    pub failover: FailoverPolicy,
    #[serde(default)]
    #[serde(alias = "review")]
    pub critic: CriticPolicy,
    #[serde(default)]
    pub memory: MemoryPolicy,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RateSampleInput {
    pub kind: String,
    pub tokens_remaining: i64,
    pub tokens_reset_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FailoverPolicy {
    #[serde(default = "default_pct", alias = "threshold_percent")]
    pub tokens_remaining_pct: f64,
    #[serde(default)]
    pub tokens_remaining_abs: Option<i64>,
    #[serde(default = "default_req")]
    pub requests_remaining: i64,
    #[serde(default = "default_chain")]
    pub chain: Vec<String>,
    #[serde(default = "yes", alias = "auto_switch")]
    pub auto_spawn: bool,
    #[serde(default)]
    pub summarize: bool,
    #[serde(default)]
    pub use_worktree: bool,
    #[serde(default)]
    pub return_to_primary: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CriticPolicy {
    /// Local agent that drives the worker role (e.g. "claude", "codex").
    #[serde(default = "default_worker")]
    pub worker_agent: String,
    #[serde(default)]
    pub worker_model: Option<String>,
    /// Local agent that drives the critic role.
    #[serde(default = "default_critic", alias = "lead_agent")]
    pub critic_agent: String,
    #[serde(default, alias = "lead_model")]
    pub critic_model: Option<String>,
    /// Local agent used for failover-snapshot summarisation.
    #[serde(default = "default_critic", alias = "summarizer_model")]
    pub summarizer_agent: String,
    #[serde(default = "default_score", alias = "passing_score")]
    pub passing_score: u32,
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemoryPolicy {
    #[serde(default = "default_memory_mode")]
    pub mode: String,
    #[serde(default = "yes")]
    pub auto_snapshot: bool,
}

fn default_max_rounds() -> u32 {
    3
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
fn default_score() -> u32 {
    8
}
fn default_memory_mode() -> String {
    "unified".into()
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
            use_worktree: false,
            return_to_primary: false,
        }
    }
}

impl Default for CriticPolicy {
    fn default() -> Self {
        Self {
            worker_agent: default_worker(),
            worker_model: None,
            critic_agent: default_critic(),
            critic_model: None,
            summarizer_agent: default_critic(),
            passing_score: default_score(),
            max_rounds: default_max_rounds(),
        }
    }
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            mode: default_memory_mode(),
            auto_snapshot: true,
        }
    }
}

pub fn load(project_root: &Path) -> Result<Policy> {
    let p = project_root.join(".handoff").join("config.toml");
    if !p.exists() {
        return Ok(Policy::default());
    }
    let body = std::fs::read_to_string(&p)?;
    let mut policy: Policy = toml::from_str(&body)?;
    policy.critic.normalize_specs();
    Ok(policy)
}

impl CriticPolicy {
    fn normalize_specs(&mut self) {
        if let Some(model) = self.worker_model.take() {
            if !self.worker_agent.contains(':') {
                self.worker_agent = format!("{}:{model}", self.worker_agent);
            }
        }
        if let Some(model) = self.critic_model.take() {
            if !self.critic_agent.contains(':') {
                self.critic_agent = format!("{}:{model}", self.critic_agent);
            }
        }
        if self.summarizer_agent == default_critic() {
            self.summarizer_agent = self.critic_agent.clone();
        }
    }
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
/// If `samples` are provided, picks the one with most tokens.
pub fn pick_next<'a>(
    chain: &'a [String],
    current: &str,
    samples: &[RateSampleInput],
) -> Option<&'a str> {
    // Candidates: in-chain agents that aren't current
    let candidates: Vec<&String> = chain.iter().filter(|k| k.as_str() != current).collect();

    if candidates.is_empty() {
        return None;
    }

    if samples.is_empty() {
        // Fallback: first candidate in chain order
        return Some(candidates[0].as_str());
    }

    // Pick the candidate with the highest tokens_remaining
    candidates
        .iter()
        .max_by_key(|k| {
            samples
                .iter()
                .find(|s| s.kind == ***k)
                .map(|s| s.tokens_remaining)
                .unwrap_or(0)
        })
        .map(|k| k.as_str())
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
        assert_eq!(pick_next(&chain, "claude", &[]), Some("codex"));
    }

    #[test]
    fn pick_next_first_when_unknown() {
        let chain = vec!["claude".to_string(), "codex".into()];
        assert_eq!(pick_next(&chain, "antigravity", &[]), Some("claude"));
    }

    #[test]
    fn review_config_combines_agent_and_model_specs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".handoff");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            r#"[review]
worker_agent = "claude"
worker_model = "haiku"
lead_agent = "codex"
lead_model = "gpt-5.5"
"#,
        )
        .unwrap();

        let policy = load(tmp.path()).unwrap();
        assert_eq!(policy.critic.worker_agent, "claude:haiku");
        assert_eq!(policy.critic.critic_agent, "codex:gpt-5.5");
        assert_eq!(policy.critic.summarizer_agent, "codex:gpt-5.5");
    }
}
