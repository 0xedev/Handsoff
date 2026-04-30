//! Cheap-worker / expensive-critic loop, driven by **local agent CLIs**.
//!
//! This is the core architectural rule of handoff: we never call provider
//! APIs directly. Both the worker and the critic are local agent CLIs that
//! the user already has installed and authenticated (`claude -p`,
//! `codex exec`, `gh copilot suggest`, …). When the local proxy is up,
//! their HTTPS calls flow through it and their token usage shows up in
//! `handoff agents` like any other client — same plumbing, no second
//! source of truth.
//!
//! Output: a `CriticResult` describing the verdict, plan, and diff. The
//! caller writes artifacts to `<project>/.handoff/scratch/critic-<ts>.{md,diff}`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::debug;

pub mod watch;

pub const WORKER_SYSTEM: &str = include_str!("worker_system.txt");
pub const CRITIC_SYSTEM: &str = include_str!("critic_system.txt");
pub const SUMMARIZER_SYSTEM: &str = include_str!("summarizer_system.txt");

pub const DEFAULT_WORKER_AGENT: &str = "claude";
pub const DEFAULT_CRITIC_AGENT: &str = "claude";

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentSpec {
    kind: String,
    model: Option<String>,
}

impl AgentSpec {
    fn parse(input: &str) -> Self {
        let trimmed = input.trim();
        if let Some((kind, model)) = trimmed.split_once(':') {
            let model = model.trim();
            Self {
                kind: kind.trim().to_ascii_lowercase(),
                model: (!model.is_empty()).then(|| model.to_string()),
            }
        } else {
            Self {
                kind: trimmed.to_ascii_lowercase(),
                model: None,
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticResult {
    pub verdict: String,
    pub plan: String,
    pub diff: String,
    pub notes: String,
    pub worker_agent: String,
    pub critic_agent: String,
    pub artifacts: Vec<String>,
}

pub mod diff;

pub struct CriticRunner {
    project_root: PathBuf,
    worker_agent: String,
    critic_agent: String,
    proxy_url: Option<String>,
    pub max_rounds: Option<u32>,
    /// Test-only: pre-canned responses keyed by `(agent, system_kind)`.
    /// `system_kind` is one of "worker", "critic", "summarizer".
    fake: Option<FakeResponses>,
}

type FakeResponses = std::sync::Mutex<std::collections::VecDeque<String>>;

impl CriticRunner {
    /// Build a runner using local agent CLIs. No API key required.
    pub fn new<P: AsRef<Path>>(project_root: P) -> Result<Self> {
        Ok(Self {
            project_root: project_root.as_ref().to_path_buf(),
            worker_agent: DEFAULT_WORKER_AGENT.into(),
            critic_agent: DEFAULT_CRITIC_AGENT.into(),
            proxy_url: Some("http://127.0.0.1:8080".into()),
            max_rounds: Some(3),
            fake: None,
        })
    }

    pub fn with_agents(mut self, worker: impl Into<String>, critic: impl Into<String>) -> Self {
        self.worker_agent = worker.into();
        self.critic_agent = critic.into();
        self
    }

    pub fn with_proxy(mut self, proxy: Option<String>) -> Self {
        self.proxy_url = proxy;
        self
    }

    /// Test-only: pre-canned response queue. Each call to `ask` pops the
    /// front of the queue.
    pub fn with_fake_responses(mut self, responses: Vec<String>) -> Self {
        self.fake = Some(std::sync::Mutex::new(responses.into()));
        self
    }

    fn proxy_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();
        if let Some(url) = &self.proxy_url {
            for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
                env.insert(k.into(), url.clone());
            }
            let ca = handoff_common::home_dir().join("ca").join("cert.pem");
            if ca.exists() {
                let s = ca.display().to_string();
                env.insert("SSL_CERT_FILE".into(), s.clone());
                env.insert("REQUESTS_CA_BUNDLE".into(), s.clone());
                env.insert("NODE_EXTRA_CA_CERTS".into(), s);
            }
        }
        env
    }

    /// Run an agent CLI with `system + user` as a single combined prompt
    /// (the local CLIs don't expose a system role over their headless
    /// flag, so we prepend the system block to the user prompt).
    async fn ask(&self, agent: &str, system: &str, user: &str) -> Result<String> {
        if let Some(fake) = &self.fake {
            let mut q = fake.lock().unwrap();
            return q
                .pop_front()
                .ok_or_else(|| anyhow!("ran out of fake responses"));
        }

        let spec = AgentSpec::parse(agent);
        let argv = headless_argv(&spec).ok_or_else(|| anyhow!("unsupported agent: {agent}"))?;
        let prompt = format!("{system}\n\n---\n\n{user}");
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .arg(&prompt)
            .current_dir(&self.project_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in self.proxy_env() {
            cmd.env(k, v);
        }
        let output = cmd
            .output()
            .await
            .with_context(|| format!("spawning {agent}"))?;
        if !output.status.success() {
            return Err(anyhow!(
                "{agent} exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Run the full worker → critic → (revise on redirect) → critic loop.
    pub async fn run(&self, task: &str) -> Result<CriticResult> {
        let max_rounds = self.max_rounds.unwrap_or(3);
        let mut feedback: Option<String> = None;
        let mut last_worker_output = String::new();

        let brain = self.read_brain();

        for round in 1..=max_rounds {
            // Worker pass
            let worker_prompt = match &feedback {
                Some(fb) => format!(
                    "## Project brain\n\n{brain}\n\n## Task\n\n{task}\n\n\
                    ## Critic feedback from round {}:\n{}\n\n\
                    Revise. Produce <plan> and <diff>.",
                    round - 1,
                    fb
                ),
                None => format!(
                    "## Project brain\n\n{brain}\n\n## Task\n\n{task}\n\nNow produce <plan> and <diff>."
                ),
            };

            last_worker_output = self
                .ask(&self.worker_agent, WORKER_SYSTEM, &worker_prompt)
                .await?;

            let plan = extract(&last_worker_output, "plan");
            let diff = extract(&last_worker_output, "diff");

            // Critic pass
            let critic_prompt = make_critic_prompt(task, &plan, &diff);
            let critic_text = self
                .ask(&self.critic_agent, CRITIC_SYSTEM, &critic_prompt)
                .await?;

            let (verdict, notes) = parse_verdict(&critic_text);

            if verdict == "approve" {
                // Extracted diffs
                let diffs = diff::extract_diffs(&last_worker_output);
                let _applied = diffs
                    .iter()
                    .all(|d| diff::apply_check(d, &self.project_root));

                let artifacts = self.write_artifacts(task, &plan, &diff, &verdict, &notes)?;
                return Ok(CriticResult {
                    verdict: "APPROVED".into(),
                    plan,
                    diff,
                    notes,
                    worker_agent: self.worker_agent.clone(),
                    critic_agent: self.critic_agent.clone(),
                    artifacts,
                });
            } else if verdict == "redirect" {
                feedback = Some(notes.clone());
                // Continue to next round
            } else {
                // Unknown verdict — treat as done to avoid infinite loop
                break;
            }
        }

        let plan = extract(&last_worker_output, "plan");
        let diff = extract(&last_worker_output, "diff");
        let artifacts = self.write_artifacts(
            task,
            &plan,
            &diff,
            "MAX_ROUNDS_REACHED",
            "Reached max rounds without approval",
        )?;

        Ok(CriticResult {
            verdict: "MAX_ROUNDS_REACHED".into(),
            plan,
            diff,
            notes: format!("Reached {max_rounds} rounds without approval"),
            worker_agent: self.worker_agent.clone(),
            critic_agent: self.critic_agent.clone(),
            artifacts,
        })
    }

    /// Critic-summarized handoff brief. Used by failover to replace the
    /// verbatim brain dump.
    pub async fn summarize_for_handoff(&self, reason: &str) -> Result<String> {
        let brain = self.read_brain();
        let scratch_blob = self.gather_recent_scratch(5);
        let mut prompt = format!(
            "## Handoff reason\n{reason}\n\n## Project brain\n{}\n",
            truncate(&brain, 4000)
        );
        if !scratch_blob.is_empty() {
            prompt.push_str(&format!(
                "\n## Recent scratch / critic notes\n{scratch_blob}\n"
            ));
        }
        prompt.push_str("\nProduce the handoff brief now.");
        self.ask(&self.critic_agent, SUMMARIZER_SYSTEM, &prompt)
            .await
    }

    fn read_brain(&self) -> String {
        let p = self.project_root.join(".handoff").join("brain.md");
        std::fs::read_to_string(p).unwrap_or_default()
    }

    fn gather_recent_scratch(&self, limit: usize) -> String {
        let scratch = self.project_root.join(".handoff").join("scratch");
        let Ok(entries) = std::fs::read_dir(&scratch) else {
            return String::new();
        };
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
            .collect();
        files.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
        files
            .iter()
            .take(limit)
            .filter_map(|e| {
                let body = std::fs::read_to_string(e.path()).ok()?;
                Some(format!(
                    "### {}\n{}",
                    e.file_name().to_string_lossy(),
                    truncate(&body, 1500)
                ))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn write_artifacts(
        &self,
        task: &str,
        plan: &str,
        diff: &str,
        verdict: &str,
        notes: &str,
    ) -> Result<Vec<String>> {
        let ts = Utc::now().timestamp();
        let scratch = self.project_root.join(".handoff").join("scratch");
        std::fs::create_dir_all(&scratch)?;
        let md_path = scratch.join(format!("critic-{ts}.md"));
        let md_body = format!(
            "# Critic run {ts}\n\nworker = {} · critic = {}\n\n\
            ## Task\n\n{task}\n\n## Verdict: {verdict}\n\n{notes}\n\n\
            ## Plan\n\n{plan}\n\n## Diff\n\n```diff\n{diff}\n```\n",
            self.worker_agent, self.critic_agent
        );
        std::fs::write(&md_path, md_body)?;
        let mut out = vec![md_path.display().to_string()];
        if !diff.trim().is_empty() {
            let diff_path = scratch.join(format!("critic-{ts}.diff"));
            let mut body = diff.to_string();
            if !body.ends_with('\n') {
                body.push('\n');
            }
            std::fs::write(&diff_path, body)?;
            out.push(diff_path.display().to_string());
        }
        Ok(out)
    }
}

/// Headless invocation per agent kind. Tail of argv is the prompt, which
/// is appended at call time.
fn headless_argv(spec: &AgentSpec) -> Option<Vec<String>> {
    let mut argv = match spec.kind.as_str() {
        "claude" => vec!["claude".into()],
        "codex" => vec!["codex".into(), "exec".into()],
        "copilot" => vec!["gh".into(), "copilot".into(), "suggest".into()],
        _ => return None,
    };
    if let Some(model) = &spec.model {
        match spec.kind.as_str() {
            "claude" | "codex" => {
                argv.push("--model".into());
                argv.push(model.clone());
            }
            _ => {}
        }
    }
    if spec.kind == "claude" {
        argv.push("-p".into());
    }
    Some(argv)
}

fn make_critic_prompt(task: &str, plan: &str, diff: &str) -> String {
    format!(
        "## Task\n{task}\n\n## Worker's plan\n{plan}\n\n## Worker's diff\n```diff\n{diff}\n```\n\n\
        Output the JSON verdict now."
    )
}

fn extract(text: &str, tag: &str) -> String {
    let pattern = format!("<{tag}>(?s)(.*?)</{tag}>");
    Regex::new(&pattern)
        .ok()
        .and_then(|re| re.captures(text))
        .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
        .unwrap_or_default()
}

fn parse_verdict(text: &str) -> (String, String) {
    for line in text.lines() {
        let l = line.trim();
        if !(l.starts_with('{') && l.ends_with('}')) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
            continue;
        };
        let verdict = v
            .get("verdict")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();
        if verdict == "approve" || verdict == "redirect" {
            let notes = v
                .get("notes")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            return (verdict, notes);
        }
    }
    debug!("critic returned malformed verdict: {text}");
    (
        "redirect".into(),
        "critic returned malformed verdict; defaulting to redirect".into(),
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(tmp: &Path) -> PathBuf {
        let h = tmp.join(".handoff");
        std::fs::create_dir_all(h.join("scratch")).unwrap();
        std::fs::write(h.join("brain.md"), "# brain\n\nstuff\n").unwrap();
        tmp.to_path_buf()
    }

    #[test]
    fn extract_handles_multiline_tags() {
        let s = "<plan>\n1. a\n2. b\n</plan>\n<diff>\nbody\n</diff>";
        assert_eq!(extract(s, "plan"), "1. a\n2. b");
        assert_eq!(extract(s, "diff"), "body");
    }

    #[test]
    fn parse_verdict_finds_approve() {
        let (v, n) = parse_verdict("blah\n{\"verdict\":\"approve\",\"notes\":\"ok\"}\n");
        assert_eq!(v, "approve");
        assert_eq!(n, "ok");
    }

    #[test]
    fn parse_verdict_redirect_default_on_garbage() {
        let (v, _) = parse_verdict("nothing structured here");
        assert_eq!(v, "redirect");
    }

    #[test]
    fn headless_argv_knows_supported_agents() {
        assert!(headless_argv(&AgentSpec::parse("claude")).is_some());
        assert!(headless_argv(&AgentSpec::parse("codex")).is_some());
        assert!(headless_argv(&AgentSpec::parse("copilot")).is_some());
        assert!(headless_argv(&AgentSpec::parse("cursor")).is_none());
    }

    #[test]
    fn headless_argv_applies_supported_model_flags() {
        assert_eq!(
            headless_argv(&AgentSpec::parse("claude:haiku")).unwrap(),
            vec!["claude", "--model", "haiku", "-p"]
        );
        assert_eq!(
            headless_argv(&AgentSpec::parse("codex:gpt-5.4")).unwrap(),
            vec!["codex", "exec", "--model", "gpt-5.4"]
        );
        assert_eq!(
            headless_argv(&AgentSpec::parse("copilot:anything")).unwrap(),
            vec!["gh", "copilot", "suggest"]
        );
    }

    #[tokio::test]
    async fn approve_path_writes_md_and_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture_root(tmp.path());
        let runner = CriticRunner::new(&root)
            .unwrap()
            .with_agents("claude", "claude")
            .with_proxy(None)
            .with_fake_responses(vec![
                "<plan>\n1. add greet\n</plan>\n<diff>\ndiff --git a/x.py b/x.py\n+def greet(): pass\n</diff>"
                    .into(),
                r#"{"verdict":"approve","notes":"ok"}"#.into(),
            ]);
        let res = runner.run("add a greet function").await.unwrap();
        assert_eq!(res.verdict, "APPROVED");
        assert!(res.diff.contains("greet"));
        assert_eq!(res.worker_agent, "claude");
        assert!(res.artifacts.iter().any(|p| p.ends_with(".md")));
        assert!(res.artifacts.iter().any(|p| p.ends_with(".diff")));
    }

    #[tokio::test]
    async fn redirect_then_approve_runs_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture_root(tmp.path());
        let runner = CriticRunner::new(&root)
            .unwrap()
            .with_agents("claude", "codex")
            .with_proxy(None)
            .with_fake_responses(vec![
                "<plan>1. wrong</plan><diff>bad</diff>".into(),
                r#"{"verdict":"redirect","notes":"wrong file"}"#.into(),
                "<plan>1. fixed</plan><diff>better</diff>".into(),
                r#"{"verdict":"approve","notes":"ok"}"#.into(),
            ]);
        let res = runner.run("do something").await.unwrap();
        assert_eq!(res.verdict, "APPROVED");
        assert!(res.plan.contains("fixed"));
        assert!(res.diff.contains("better"));
        assert_eq!(res.worker_agent, "claude");
        assert_eq!(res.critic_agent, "codex");
    }

    #[tokio::test]
    async fn empty_diff_skips_diff_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture_root(tmp.path());
        let runner = CriticRunner::new(&root)
            .unwrap()
            .with_agents("claude", "claude")
            .with_proxy(None)
            .with_fake_responses(vec![
                "<plan>1. explore</plan><diff></diff>".into(),
                r#"{"verdict":"approve","notes":"nothing to do"}"#.into(),
            ]);
        let res = runner.run("explore").await.unwrap();
        assert!(res.artifacts.iter().any(|p| p.ends_with(".md")));
        assert!(!res.artifacts.iter().any(|p| p.ends_with(".diff")));
    }

    #[tokio::test]
    async fn unsupported_agent_errors_clearly() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture_root(tmp.path());
        let runner = CriticRunner::new(&root)
            .unwrap()
            .with_agents("antigravity", "claude")
            .with_proxy(None);
        let err = runner.run("anything").await.unwrap_err().to_string();
        assert!(err.contains("antigravity") || err.contains("unsupported"));
    }
}
