//! Per-agent adapters: detection, header parsing, context-file targets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::DateTime;
use handoff_common::{AgentKind, RateSample};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessMatch {
    pub pid: i64,
    pub name: String,
    pub cmdline: Vec<String>,
}

/// Process info as we get it from `sysinfo`.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: i64,
    pub name: String,
    pub cmdline: Vec<String>,
}

pub trait Adapter: Send + Sync {
    fn kind(&self) -> AgentKind;
    fn binaries(&self) -> &'static [&'static str];
    fn api_hosts(&self) -> &'static [&'static str];

    /// Default: match by binary name OR by `Path::file_name(cmdline[0])`.
    fn detect(&self, procs: &[ProcInfo]) -> Vec<ProcessMatch> {
        let bins: Vec<String> = self
            .binaries()
            .iter()
            .map(|b| b.to_ascii_lowercase())
            .collect();
        let mut out = Vec::new();
        for p in procs {
            if p.cmdline.is_empty() {
                continue;
            }
            let head = Path::new(&p.cmdline[0])
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let name = p.name.to_ascii_lowercase();
            if bins.iter().any(|b| b == &head || b == &name) {
                out.push(ProcessMatch {
                    pid: p.pid,
                    name: p.name.clone(),
                    cmdline: p.cmdline.clone(),
                });
            }
        }
        out
    }

    /// True if this adapter's API hosts include `host`.
    fn classify_host(&self, host: &str) -> bool {
        let h = host.to_ascii_lowercase();
        self.api_hosts()
            .iter()
            .any(|api| h == *api || h.ends_with(&format!(".{}", api)))
    }

    /// Files this agent reads as project context (relative to project root).
    fn context_files(&self, project_root: &Path) -> Vec<PathBuf>;

    /// Parse rate-limit headers; return None if not applicable.
    fn parse_headers(&self, headers: &BTreeMap<String, String>) -> Option<RateSample>;

    /// Returns arguments to append to the resolved binary for headless execution.
    fn headless_args(&self, _prompt: &str) -> Option<Vec<String>> {
        None
    }
}

/// Parse a header that's either ISO-8601, a duration like "5s", or epoch/seconds.
pub fn parse_reset_epoch(value: &str) -> Option<i64> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if v.contains('T') {
        if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
            return Some(dt.timestamp());
        }
    }
    if let Some(last) = v.chars().last() {
        if matches!(last, 's' | 'm' | 'h') {
            let mut total = 0i64;
            let mut buf = String::new();
            for ch in v.chars() {
                if ch.is_ascii_digit() || ch == '.' {
                    buf.push(ch);
                } else if matches!(ch, 's' | 'm' | 'h') && !buf.is_empty() {
                    let n: f64 = buf.parse().unwrap_or(0.0);
                    let mult = match ch {
                        's' => 1,
                        'm' => 60,
                        'h' => 3600,
                        _ => 1,
                    };
                    total += (n as i64) * mult;
                    buf.clear();
                }
            }
            if total > 0 {
                let now = chrono::Utc::now().timestamp();
                return Some(now + total);
            }
        }
    }
    if let Ok(n) = v.parse::<i64>() {
        if n > 1_000_000_000 {
            return Some(n);
        }
        return Some(chrono::Utc::now().timestamp() + n);
    }
    None
}

fn parse_int(headers: &BTreeMap<String, String>, key: &str) -> Option<i64> {
    headers
        .get(key)
        .or_else(|| headers.get(&key.to_ascii_lowercase()))
        .and_then(|v| v.trim().parse().ok())
}

// --- Concrete adapters ----------------------------------------------------

pub struct ClaudeAdapter;
pub struct CodexAdapter;
pub struct CopilotAdapter;
pub struct CursorAdapter;
pub struct GeminiAdapter;
pub struct AiderAdapter;
pub struct ClineAdapter;

impl Adapter for ClaudeAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Claude
    }
    fn binaries(&self) -> &'static [&'static str] {
        &["claude", "claude-code"]
    }
    fn api_hosts(&self) -> &'static [&'static str] {
        &["api.anthropic.com"]
    }
    fn context_files(&self, root: &Path) -> Vec<PathBuf> {
        vec![root.join("CLAUDE.md")]
    }
    fn parse_headers(&self, headers: &BTreeMap<String, String>) -> Option<RateSample> {
        let lc: BTreeMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        if !lc.keys().any(|k| k.starts_with("anthropic-ratelimit-")) {
            return None;
        }
        let mut raw = serde_json::Map::new();
        for (k, v) in &lc {
            if k.starts_with("anthropic-ratelimit-") {
                raw.insert(k.clone(), serde_json::Value::String(v.clone()));
            }
        }
        Some(RateSample {
            provider: "anthropic".into(),
            tokens_remaining: parse_int(&lc, "anthropic-ratelimit-tokens-remaining"),
            requests_remaining: parse_int(&lc, "anthropic-ratelimit-requests-remaining"),
            tokens_reset_at: lc
                .get("anthropic-ratelimit-tokens-reset")
                .and_then(|v| parse_reset_epoch(v)),
            requests_reset_at: lc
                .get("anthropic-ratelimit-requests-reset")
                .and_then(|v| parse_reset_epoch(v)),
            raw_headers: raw,
        })
    }
    fn headless_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec!["-p".into(), prompt.into()])
    }
}

impl Adapter for CodexAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }
    fn binaries(&self) -> &'static [&'static str] {
        &["codex"]
    }
    fn api_hosts(&self) -> &'static [&'static str] {
        &["api.openai.com"]
    }
    fn context_files(&self, root: &Path) -> Vec<PathBuf> {
        vec![root.join("AGENTS.md")]
    }
    fn parse_headers(&self, headers: &BTreeMap<String, String>) -> Option<RateSample> {
        let lc: BTreeMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        if !lc.keys().any(|k| k.starts_with("x-ratelimit-")) {
            return None;
        }
        let mut raw = serde_json::Map::new();
        for (k, v) in &lc {
            if k.starts_with("x-ratelimit-") {
                raw.insert(k.clone(), serde_json::Value::String(v.clone()));
            }
        }
        Some(RateSample {
            provider: "openai".into(),
            tokens_remaining: parse_int(&lc, "x-ratelimit-remaining-tokens"),
            requests_remaining: parse_int(&lc, "x-ratelimit-remaining-requests"),
            tokens_reset_at: lc
                .get("x-ratelimit-reset-tokens")
                .and_then(|v| parse_reset_epoch(v)),
            requests_reset_at: lc
                .get("x-ratelimit-reset-requests")
                .and_then(|v| parse_reset_epoch(v)),
            raw_headers: raw,
        })
    }
    fn headless_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec!["exec".into(), prompt.into()])
    }
}

impl Adapter for CopilotAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Copilot
    }
    fn binaries(&self) -> &'static [&'static str] {
        &["gh", "copilot"]
    }
    fn api_hosts(&self) -> &'static [&'static str] {
        &["api.githubcopilot.com", "api.github.com"]
    }
    fn detect(&self, procs: &[ProcInfo]) -> Vec<ProcessMatch> {
        let mut out = Vec::new();
        for p in procs {
            if p.cmdline.is_empty() {
                continue;
            }
            let head = Path::new(&p.cmdline[0])
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if head == "copilot" {
                out.push(ProcessMatch {
                    pid: p.pid,
                    name: "copilot".into(),
                    cmdline: p.cmdline.clone(),
                });
            } else if head == "gh"
                && p.cmdline.len() > 1
                && p.cmdline[1].eq_ignore_ascii_case("copilot")
            {
                out.push(ProcessMatch {
                    pid: p.pid,
                    name: "gh-copilot".into(),
                    cmdline: p.cmdline.clone(),
                });
            }
        }
        out
    }
    fn context_files(&self, root: &Path) -> Vec<PathBuf> {
        vec![root.join(".github").join("copilot-instructions.md")]
    }
    fn parse_headers(&self, _headers: &BTreeMap<String, String>) -> Option<RateSample> {
        None
    }
    fn headless_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec!["copilot".into(), "suggest".into(), prompt.into()])
    }
}

impl Adapter for CursorAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Cursor
    }
    fn binaries(&self) -> &'static [&'static str] {
        &["cursor", "antigravity", "Cursor", "Antigravity"]
    }
    fn api_hosts(&self) -> &'static [&'static str] {
        &[]
    }
    fn detect(&self, procs: &[ProcInfo]) -> Vec<ProcessMatch> {
        let targets: Vec<String> = self
            .binaries()
            .iter()
            .map(|b| b.to_ascii_lowercase())
            .collect();
        let mut out = Vec::new();
        for p in procs {
            if p.cmdline.is_empty() {
                continue;
            }
            if p.cmdline.iter().any(|a| a.starts_with("--type=")) {
                continue;
            }
            let head = Path::new(&p.cmdline[0])
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if targets.contains(&head) || targets.contains(&p.name.to_ascii_lowercase()) {
                out.push(ProcessMatch {
                    pid: p.pid,
                    name: p.name.clone(),
                    cmdline: p.cmdline.clone(),
                });
            }
        }
        out
    }
    fn context_files(&self, root: &Path) -> Vec<PathBuf> {
        vec![root.join(".cursorrules")]
    }
    fn parse_headers(&self, _headers: &BTreeMap<String, String>) -> Option<RateSample> {
        None
    }
}

impl Adapter for GeminiAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Gemini
    }
    fn binaries(&self) -> &'static [&'static str] {
        &["gemini"]
    }
    fn api_hosts(&self) -> &'static [&'static str] {
        &["generativelanguage.googleapis.com"]
    }
    fn context_files(&self, root: &Path) -> Vec<PathBuf> {
        vec![root.join("GEMINI.md")]
    }
    fn parse_headers(&self, _headers: &BTreeMap<String, String>) -> Option<RateSample> {
        None
    }
    fn headless_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec!["-p".into(), prompt.into()])
    }
}

impl Adapter for AiderAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Aider
    }
    fn binaries(&self) -> &'static [&'static str] {
        &["aider"]
    }
    fn api_hosts(&self) -> &'static [&'static str] {
        &["api.openai.com", "api.anthropic.com"]
    }
    fn context_files(&self, root: &Path) -> Vec<PathBuf> {
        vec![root.join(".aider.conf.yml")]
    }
    fn parse_headers(&self, _headers: &BTreeMap<String, String>) -> Option<RateSample> {
        None
    }
    fn headless_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec!["--message".into(), prompt.into(), "--yes".into()])
    }
}

impl Adapter for ClineAdapter {
    fn kind(&self) -> AgentKind {
        AgentKind::Cline
    }
    fn binaries(&self) -> &'static [&'static str] {
        &["cline"]
    }
    fn api_hosts(&self) -> &'static [&'static str] {
        &[]
    }
    fn context_files(&self, root: &Path) -> Vec<PathBuf> {
        vec![root.join(".clinerules")]
    }
    fn parse_headers(&self, _headers: &BTreeMap<String, String>) -> Option<RateSample> {
        None
    }
    fn headless_args(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec!["-p".into(), prompt.into()])
    }
}

pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(ClaudeAdapter),
        Box::new(CodexAdapter),
        Box::new(CopilotAdapter),
        Box::new(CursorAdapter),
        Box::new(GeminiAdapter),
        Box::new(AiderAdapter),
        Box::new(ClineAdapter),
    ]
}

pub fn for_kind(kind: AgentKind) -> Box<dyn Adapter> {
    match kind {
        AgentKind::Claude => Box::new(ClaudeAdapter),
        AgentKind::Codex => Box::new(CodexAdapter),
        AgentKind::Copilot => Box::new(CopilotAdapter),
        AgentKind::Cursor => Box::new(CursorAdapter),
        AgentKind::Gemini => Box::new(GeminiAdapter),
        AgentKind::Aider => Box::new(AiderAdapter),
        AgentKind::Cline => Box::new(ClineAdapter),
    }
}

/// Look up an adapter by its string name. Returns `None` for unknown kinds.
pub fn for_kind_str(kind: &str) -> Option<Box<dyn Adapter>> {
    AgentKind::parse(kind).map(for_kind)
}

/// Snapshot the running process table via `sysinfo`.
pub fn snapshot_procs() -> Vec<ProcInfo> {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All);
    sys.processes()
        .iter()
        .map(|(pid, proc)| ProcInfo {
            pid: pid.as_u32() as i64,
            name: proc.name().to_string_lossy().into_owned(),
            cmdline: proc
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i64, name: &str, cmd: &[&str]) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            cmdline: cmd.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn claude_detects_binary() {
        let procs = vec![
            proc(1, "bash", &["/bin/bash"]),
            proc(42, "claude", &["/usr/local/bin/claude", "--help"]),
        ];
        let m = ClaudeAdapter.detect(&procs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].pid, 42);
    }

    #[test]
    fn copilot_requires_subcommand() {
        let procs = vec![
            proc(100, "gh", &["/usr/bin/gh", "issue", "list"]),
            proc(101, "gh", &["/usr/bin/gh", "copilot", "suggest"]),
        ];
        let m = CopilotAdapter.detect(&procs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].pid, 101);
    }

    #[test]
    fn cursor_skips_electron_children() {
        let procs = vec![
            proc(
                200,
                "Cursor",
                &["/Applications/Cursor.app/Contents/MacOS/Cursor"],
            ),
            proc(
                201,
                "Cursor Helper",
                &[
                    "/Applications/Cursor.app/Contents/Frameworks/Helper",
                    "--type=renderer",
                ],
            ),
        ];
        let m = CursorAdapter.detect(&procs);
        let pids: Vec<i64> = m.iter().map(|x| x.pid).collect();
        assert!(pids.contains(&200));
        assert!(!pids.contains(&201));
    }

    #[test]
    fn classify_host_works() {
        assert!(ClaudeAdapter.classify_host("api.anthropic.com"));
        assert!(!ClaudeAdapter.classify_host("api.openai.com"));
        assert!(CodexAdapter.classify_host("api.openai.com"));
    }

    #[test]
    fn anthropic_headers_parse() {
        let mut h = BTreeMap::new();
        h.insert(
            "anthropic-ratelimit-tokens-remaining".into(),
            "120000".into(),
        );
        h.insert(
            "anthropic-ratelimit-tokens-reset".into(),
            "2026-04-26T10:00:00Z".into(),
        );
        h.insert("anthropic-ratelimit-requests-remaining".into(), "47".into());
        let s = ClaudeAdapter.parse_headers(&h).unwrap();
        assert_eq!(s.provider, "anthropic");
        assert_eq!(s.tokens_remaining, Some(120000));
        assert_eq!(s.requests_remaining, Some(47));
        assert!(s.tokens_reset_at.unwrap() > 1_000_000_000);
    }

    #[test]
    fn openai_headers_parse_duration_reset() {
        let mut h = BTreeMap::new();
        h.insert("x-ratelimit-remaining-tokens".into(), "98000".into());
        h.insert("x-ratelimit-remaining-requests".into(), "499".into());
        h.insert("x-ratelimit-reset-requests".into(), "60s".into());
        let s = CodexAdapter.parse_headers(&h).unwrap();
        assert_eq!(s.provider, "openai");
        assert_eq!(s.tokens_remaining, Some(98000));
        assert!(s.requests_reset_at.unwrap() > chrono::Utc::now().timestamp());
    }

    #[test]
    fn copilot_returns_none() {
        let mut h = BTreeMap::new();
        h.insert("foo".into(), "bar".into());
        assert!(CopilotAdapter.parse_headers(&h).is_none());
    }
}
