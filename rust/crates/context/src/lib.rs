//! Context engine: project scaffold + intelligent handoff snapshots.
//!
//! Design split (per user feedback):
//! - **usage tracking** says *when* to switch (lives in storage + policy).
//! - **handoff quality** says *how to continue* — that's this crate.
//!
//! A snapshot is not a brain dump. It is a structured record of:
//!   * git state (diff, HEAD, files touched),
//!   * recent shell activity,
//!   * failing tests,
//!   * the current objective + blocker + next action,
//!   * a "do not touch" list,
//!   * an optional critic-summarized brief.
//!
//! The output is both machine-readable JSON (so the next agent can parse
//! it) and a human-readable Markdown rendering.

pub mod sources;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use handoff_adapters::all as all_adapters;
use handoff_common::Snapshot;

mod intent;
pub use intent::SnapshotIntentSource;

const DEFAULT_BRAIN: &str = "\
# Project brain

This is the canonical context for every AI agent working in this project.
Edit it directly. `handoff sync` mirrors it into each agent's native context
file (CLAUDE.md, AGENTS.md, .cursorrules, copilot-instructions.md).

## Goals
-

## Architecture
-

## Conventions
-

## Open questions
-
";

const DEFAULT_INTENT: &str = "\
# Intent

Tell the next agent exactly how to continue. Edit these blocks as you work.

```toml
current_objective = \"\"
blocker = \"\"
next_action = \"\"
do_not_touch = []
```
";

const DEFAULT_CONFIG: &str = include_str!("default_config.toml");

const FRONTMATTER: &str = "---";

fn strip_frontmatter(text: &str) -> &str {
    let bytes = text.as_bytes();
    if !text.starts_with(FRONTMATTER) {
        return text;
    }
    // find the second `---\n`
    if let Some(end) = text[FRONTMATTER.len()..].find("\n---\n") {
        let cut = FRONTMATTER.len() + end + "\n---\n".len();
        return std::str::from_utf8(&bytes[cut..]).unwrap_or(text);
    }
    text
}

/// Scaffold `.handoff/` in the project root. Idempotent.
pub fn init_project(root: &Path) -> Result<PathBuf> {
    let h = root.join(".handoff");
    std::fs::create_dir_all(h.join("decisions"))?;
    std::fs::create_dir_all(h.join("scratch"))?;
    std::fs::create_dir_all(h.join("derived"))?;
    let brain = h.join("brain.md");
    if !brain.exists() {
        std::fs::write(&brain, DEFAULT_BRAIN)?;
    }
    let intent = h.join("intent.md");
    if !intent.exists() {
        std::fs::write(&intent, DEFAULT_INTENT)?;
    }
    let config = h.join("config.toml");
    if !config.exists() {
        std::fs::write(&config, DEFAULT_CONFIG)?;
    }
    let gi = h.join(".gitignore");
    if !gi.exists() {
        std::fs::write(&gi, "derived/\nscratch/\n")?;
    }
    Ok(h)
}

pub struct ContextEngine {
    pub root: PathBuf,
}

impl ContextEngine {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn handoff_dir(&self) -> PathBuf {
        self.root.join(".handoff")
    }

    fn brain_path(&self) -> PathBuf {
        self.handoff_dir().join("brain.md")
    }

    fn intent_path(&self) -> PathBuf {
        self.handoff_dir().join("intent.md")
    }

    /// Render `brain.md` into each adapter's native context file.
    pub fn sync(&self) -> Result<Vec<PathBuf>> {
        let brain_path = self.brain_path();
        let body = std::fs::read_to_string(&brain_path)
            .with_context(|| format!("no brain at {} (run `handoff init`)", brain_path.display()))?;
        let body = strip_frontmatter(&body).to_string();
        let mut written = Vec::new();
        for adapter in all_adapters() {
            for target in adapter.context_files(&self.root) {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, &body)?;
                written.push(target);
            }
        }
        Ok(written)
    }

    /// Build an intelligent Snapshot. Reads git state, intent.md, the most
    /// recent test logs (if any), and brain.md. This is the heart of the app.
    pub fn snapshot(&self, reason: &str) -> Result<(Snapshot, PathBuf)> {
        let now = Utc::now().timestamp();

        let git_diff = sources::git::diff_head(&self.root).unwrap_or_default();
        let files_touched = sources::git::files_touched(&self.root).unwrap_or_default();
        let git_head = sources::git::head_info(&self.root);
        let recent_commands = sources::shell::recent_commands(&self.root, 20);
        let failing_tests = sources::tests::failing_from_scratch(&self.handoff_dir().join("scratch"));
        let intent = SnapshotIntentSource::load(&self.intent_path()).unwrap_or_default();

        let snap = Snapshot {
            generated_at: now,
            project_root: self.root.display().to_string(),
            reason: reason.to_string(),
            git_diff: truncate(&git_diff, 12_000),
            files_touched,
            git_head,
            recent_commands,
            failing_tests,
            current_objective: intent.current_objective,
            blocker: intent.blocker,
            next_action: intent.next_action,
            do_not_touch: intent.do_not_touch,
            critic_brief: None,
        };

        // Persist both .json (machine) and .md (human).
        let scratch = self.handoff_dir().join("scratch");
        std::fs::create_dir_all(&scratch)?;
        let stem = format!("handoff-{}", now);
        let json_path = scratch.join(format!("{stem}.json"));
        let md_path = scratch.join(format!("{stem}.md"));
        std::fs::write(&json_path, serde_json::to_string_pretty(&snap)?)?;
        std::fs::write(&md_path, render_markdown(&snap))?;
        Ok((snap, md_path))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut head = s[..max.saturating_sub(64)].to_string();
    head.push_str(&format!(
        "\n\n... [truncated {} bytes — see git diff HEAD locally for full output]\n",
        s.len() - max
    ));
    head
}

/// Render a Snapshot as Markdown. The new agent reads this directly.
pub fn render_markdown(s: &Snapshot) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Handoff snapshot {}\n\n", s.generated_at));
    out.push_str(&format!("**Reason:** {}\n\n", s.reason));
    out.push_str(&format!("**Project:** `{}`\n\n", s.project_root));

    out.push_str("## Current objective\n\n");
    out.push_str(s.current_objective.as_deref().unwrap_or("_(unset — populate `.handoff/intent.md`)_"));
    out.push_str("\n\n");

    out.push_str("## Next action\n\n");
    out.push_str(s.next_action.as_deref().unwrap_or("_(unset)_"));
    out.push_str("\n\n");

    if let Some(b) = &s.blocker {
        out.push_str("## Blocker\n\n");
        out.push_str(b);
        out.push_str("\n\n");
    }

    if !s.do_not_touch.is_empty() {
        out.push_str("## Do NOT touch\n\n");
        for p in &s.do_not_touch {
            out.push_str(&format!("- `{}`\n", p));
        }
        out.push('\n');
    }

    if let Some(h) = &s.git_head {
        out.push_str(&format!(
            "## Git HEAD\n\n`{}` on `{}` — {}\n\n",
            &h.sha[..h.sha.len().min(12)],
            h.branch,
            h.message
        ));
    }

    if !s.files_touched.is_empty() {
        out.push_str("## Files touched (uncommitted)\n\n");
        for f in &s.files_touched {
            out.push_str(&format!("- `{}`\n", f));
        }
        out.push('\n');
    }

    if !s.git_diff.trim().is_empty() {
        out.push_str("## Working diff (`git diff HEAD`)\n\n```diff\n");
        out.push_str(&s.git_diff);
        if !s.git_diff.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n\n");
    }

    if !s.failing_tests.is_empty() {
        out.push_str("## Failing tests\n\n");
        for t in &s.failing_tests {
            out.push_str(&format!("- **{}**", t.name));
            if let Some(f) = &t.file {
                out.push_str(&format!(" (`{}`)", f));
            }
            out.push_str(": ");
            out.push_str(&t.message);
            out.push('\n');
        }
        out.push('\n');
    }

    if !s.recent_commands.is_empty() {
        out.push_str("## Recent commands\n\n```\n");
        for c in &s.recent_commands {
            out.push_str(c);
            out.push('\n');
        }
        out.push_str("```\n\n");
    }

    if let Some(b) = &s.critic_brief {
        out.push_str("## Critic brief\n\n");
        out.push_str(b);
        out.push_str("\n\n");
    }

    out
}
