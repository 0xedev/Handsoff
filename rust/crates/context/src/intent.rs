//! Parser for `.handoff/intent.md` — the structured "what's next" doc.
//!
//! The file is mostly free-form Markdown for humans, but contains one
//! TOML fenced code block we extract:
//!
//! ```toml
//! current_objective = "..."
//! blocker = "..."
//! next_action = "..."
//! do_not_touch = ["src/legacy/", "migrations/"]
//! ```

use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
pub struct SnapshotIntentSource {
    #[serde(default)]
    pub current_objective: Option<String>,
    #[serde(default)]
    pub blocker: Option<String>,
    #[serde(default)]
    pub next_action: Option<String>,
    #[serde(default)]
    pub do_not_touch: Vec<String>,
}

impl SnapshotIntentSource {
    pub fn load(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path)?;
        Ok(parse(&body))
    }
}

pub fn parse(body: &str) -> SnapshotIntentSource {
    if let Some(toml_block) = extract_toml_block(body) {
        if let Ok(src) = toml::from_str::<SnapshotIntentSource>(&toml_block) {
            return normalize(src);
        }
    }
    SnapshotIntentSource::default()
}

fn normalize(mut s: SnapshotIntentSource) -> SnapshotIntentSource {
    let blank = |o: &Option<String>| o.as_deref().map(|v| v.trim().is_empty()).unwrap_or(true);
    if blank(&s.current_objective) {
        s.current_objective = None;
    }
    if blank(&s.blocker) {
        s.blocker = None;
    }
    if blank(&s.next_action) {
        s.next_action = None;
    }
    s.do_not_touch.retain(|x| !x.trim().is_empty());
    s
}

fn extract_toml_block(body: &str) -> Option<String> {
    let mut lines = body.lines();
    let mut in_block = false;
    let mut buf = String::new();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if !in_block && (trimmed == "```toml" || trimmed.starts_with("```toml ")) {
            in_block = true;
            continue;
        }
        if in_block && trimmed.starts_with("```") {
            return Some(buf);
        }
        if in_block {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_intent_block() {
        let body = r#"# Intent

```toml
current_objective = "ship the proxy"
blocker = "no rust mitm crate"
next_action = "evaluate hudsucker"
do_not_touch = ["src/legacy/", "migrations/"]
```
"#;
        let s = parse(body);
        assert_eq!(s.current_objective.as_deref(), Some("ship the proxy"));
        assert_eq!(s.next_action.as_deref(), Some("evaluate hudsucker"));
        assert_eq!(s.do_not_touch.len(), 2);
    }

    #[test]
    fn empty_strings_become_none() {
        let body = "```toml\ncurrent_objective = \"\"\nnext_action = \"\"\n```";
        let s = parse(body);
        assert!(s.current_objective.is_none());
        assert!(s.next_action.is_none());
    }

    #[test]
    fn missing_block_yields_default() {
        let s = parse("# random markdown\n\nno toml here");
        assert!(s.current_objective.is_none());
        assert!(s.do_not_touch.is_empty());
    }
}
