use serde::{Deserialize, Serialize};

/// Provider whose rate-limit headers we know how to parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    Openai,
    Github,
    Google,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::Openai => "openai",
            Provider::Github => "github",
            Provider::Google => "google",
        }
    }
}

/// One usage observation extracted from a response. Mirrors the v0.x Python
/// `RateSample` field-for-field so we can write to the same SQLite DB.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RateSample {
    pub provider: String,
    pub tokens_remaining: Option<i64>,
    pub requests_remaining: Option<i64>,
    pub tokens_reset_at: Option<i64>,
    pub requests_reset_at: Option<i64>,
    #[serde(default)]
    pub raw_headers: serde_json::Map<String, serde_json::Value>,
}

/// Logical kinds of agents handoff knows about. Stored as TEXT in SQLite,
/// so adding a new variant is non-breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Copilot,
    Cursor,
    Gemini,
    Aider,
    Cline,
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Copilot => "copilot",
            AgentKind::Cursor => "cursor",
            AgentKind::Gemini => "gemini",
            AgentKind::Aider => "aider",
            AgentKind::Cline => "cline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "copilot" | "gh-copilot" => Some(Self::Copilot),
            "cursor" | "antigravity" => Some(Self::Cursor),
            "gemini" => Some(Self::Gemini),
            "aider" => Some(Self::Aider),
            "cline" => Some(Self::Cline),
            _ => None,
        }
    }
}

/// A handoff snapshot — the *intelligent* version. This is the heart of the
/// app: everything the next agent needs to continue, in a structured form.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub generated_at: i64,
    pub project_root: String,
    pub reason: String,

    /// Output of `git diff HEAD` (truncated).
    pub git_diff: String,
    /// Files in the working tree with uncommitted changes.
    pub files_touched: Vec<String>,
    /// HEAD commit + branch info for the new agent to base its work on.
    pub git_head: Option<GitHead>,

    /// Last N shell commands the user/agent ran (best-effort).
    pub recent_commands: Vec<String>,
    /// Test failures parsed from the last test run, if any.
    pub failing_tests: Vec<TestFailure>,

    /// One-line goal of the current work block.
    pub current_objective: Option<String>,
    /// What's stuck right now, in plain words.
    pub blocker: Option<String>,
    /// The exact next thing to do — phrased as an imperative for the next agent.
    pub next_action: Option<String>,
    /// Files / directories the next agent must NOT modify.
    pub do_not_touch: Vec<String>,

    /// Optional critic-summarized brief (when summarize=true in policy).
    pub critic_brief: Option<String>,

    pub git_log: Option<String>,
    pub untracked_files: Vec<String>,
    pub conversation_tail: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GitHead {
    pub branch: String,
    pub sha: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    pub name: String,
    pub file: Option<String>,
    pub message: String,
}
