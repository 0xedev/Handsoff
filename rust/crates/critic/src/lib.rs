//! Cheap-worker / expensive-critic loop, full Rust port.
//!
//! Calls the Anthropic Messages API directly via `reqwest`. When configured
//! with a `proxy_url`, all requests route through the local mitm proxy so
//! token usage shows up in `handoff agents` like any other client.
//!
//! Output: a `CriticResult` describing the verdict, plan, and diff. The
//! caller writes artifacts to `<project>/.handoff/scratch/critic-<ts>.{md,diff}`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

pub mod watch;

pub const WORKER_SYSTEM: &str = include_str!("worker_system.txt");
pub const CRITIC_SYSTEM: &str = include_str!("critic_system.txt");
pub const SUMMARIZER_SYSTEM: &str = include_str!("summarizer_system.txt");

pub const DEFAULT_WORKER_MODEL: &str = "claude-haiku-4-5-20251001";
pub const DEFAULT_CRITIC_MODEL: &str = "claude-opus-4-7";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";

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

/// Builder/runtime for one critic loop run.
pub struct CriticRunner {
    project_root: PathBuf,
    worker_model: String,
    critic_model: String,
    proxy_url: Option<String>,
    api_key: String,
    /// For tests: an injected one-shot client that yields canned `(text, tokens)` per call.
    fake: Option<FakeAsk>,
}

type FakeAsk = std::sync::Mutex<std::vec::IntoIter<(String, u64)>>;

impl CriticRunner {
    pub fn new<P: AsRef<Path>>(project_root: P) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY not set")?;
        Ok(Self {
            project_root: project_root.as_ref().to_path_buf(),
            worker_model: DEFAULT_WORKER_MODEL.into(),
            critic_model: DEFAULT_CRITIC_MODEL.into(),
            proxy_url: Some("http://127.0.0.1:8080".into()),
            api_key,
            fake: None,
        })
    }

    pub fn with_models(mut self, worker: impl Into<String>, critic: impl Into<String>) -> Self {
        self.worker_model = worker.into();
        self.critic_model = critic.into();
        self
    }

    pub fn with_proxy(mut self, proxy: Option<String>) -> Self {
        self.proxy_url = proxy;
        self
    }

    /// Test-only: install a fake `_ask` that returns canned responses in order.
    pub fn with_fake_responses(mut self, responses: Vec<(String, u64)>) -> Self {
        self.fake = Some(std::sync::Mutex::new(responses.into_iter()));
        self.api_key = "fake".into();
        self
    }

    fn http_client(&self) -> Result<Client> {
        let mut b = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .danger_accept_invalid_certs(true); // mitmproxy CA may not be system-trusted
        if let Some(p) = &self.proxy_url {
            b = b.proxy(reqwest::Proxy::all(p)?);
        }
        Ok(b.build()?)
    }

    /// One-shot Anthropic Messages call. Returns (assistant_text, total_tokens).
    async fn ask(&self, model: &str, system: &str, user: &str) -> Result<(String, u64)> {
        if let Some(fake) = &self.fake {
            let mut g = fake.lock().unwrap();
            return g
                .next()
                .ok_or_else(|| anyhow!("ran out of fake responses"));
        }

        let client = self.http_client()?;
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 2048,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        });
        let resp = client
            .post(ANTHROPIC_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("anthropic request failed")?;

        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "anthropic {}: {}",
                status,
                String::from_utf8_lossy(&bytes)
            ));
        }
        let parsed: AnthropicResponse = serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding anthropic response: {}",
                String::from_utf8_lossy(&bytes)))?;
        let text = parsed
            .content
            .into_iter()
            .filter_map(|b| if b.kind == "text" { Some(b.text) } else { None })
            .collect::<Vec<_>>()
            .join("");
        let tokens = parsed.usage.input_tokens + parsed.usage.output_tokens;
        Ok((text, tokens))
    }

    /// Run the full worker → critic → (revise on redirect) → critic loop.
    pub async fn run(&self, task: &str) -> Result<CriticResult> {
        let brain = self.read_brain();

        let worker_prompt = format!(
            "## Project brain\n\n{brain}\n\n## Task\n\n{task}\n\nNow produce <plan> and <diff>."
        );
        let (worker_text, mut worker_tokens) = self
            .ask(&self.worker_model, WORKER_SYSTEM, &worker_prompt)
            .await?;
        let mut plan = extract(&worker_text, "plan");
        let mut diff = extract(&worker_text, "diff");

        let critic_prompt = make_critic_prompt(task, &plan, &diff);
        let (critic_text, mut critic_tokens) = self
            .ask(&self.critic_model, CRITIC_SYSTEM, &critic_prompt)
            .await?;
        let (mut verdict, mut notes) = parse_verdict(&critic_text);

        if verdict == "redirect" {
            let revise_prompt = format!(
                "{worker_prompt}\n\n## Critic feedback (your previous attempt was rejected)\n\
                {notes}\n\nRevise. Produce <plan> and <diff> again."
            );
            let (worker_text2, w2) = self
                .ask(&self.worker_model, WORKER_SYSTEM, &revise_prompt)
                .await?;
            worker_tokens += w2;
            let new_plan = extract(&worker_text2, "plan");
            let new_diff = extract(&worker_text2, "diff");
            if !new_plan.is_empty() {
                plan = new_plan;
            }
            if !new_diff.is_empty() {
                diff = new_diff;
            }

            let critic_prompt2 = make_critic_prompt(task, &plan, &diff);
            let (critic_text2, c2) = self
                .ask(&self.critic_model, CRITIC_SYSTEM, &critic_prompt2)
                .await?;
            critic_tokens += c2;
            let parsed = parse_verdict(&critic_text2);
            verdict = parsed.0;
            notes = parsed.1;
        }

        let artifacts = self.write_artifacts(task, &plan, &diff, &verdict, &notes)?;
        Ok(CriticResult {
            verdict,
            plan,
            diff,
            notes,
            worker_tokens,
            critic_tokens,
            artifacts,
        })
    }

    /// Critic-summarized handoff brief. Used by failover to replace the
    /// verbatim brain dump.
    pub async fn summarize_for_handoff(&self, reason: &str) -> Result<(String, u64)> {
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
        self.ask(&self.critic_model, SUMMARIZER_SYSTEM, &prompt).await
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
        files.sort_by_key(|e| {
            std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok())
        });
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
            "# Critic run {ts}\n\n## Task\n\n{task}\n\n## Verdict: {verdict}\n\n{notes}\n\n\
            ## Plan\n\n{plan}\n\n## Diff\n\n```diff\n{diff}\n```\n"
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
        out.push_str("…");
        out
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicBlock>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
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

    #[tokio::test]
    async fn approve_path_writes_md_and_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture_root(tmp.path());
        let runner = CriticRunner {
            project_root: root.clone(),
            worker_model: "w".into(),
            critic_model: "c".into(),
            proxy_url: None,
            api_key: "fake".into(),
            fake: None,
        }
        .with_fake_responses(vec![
            (
                "<plan>\n1. add greet\n</plan>\n<diff>\ndiff --git a/x.py b/x.py\n+def greet(): pass\n</diff>"
                    .into(),
                100,
            ),
            (r#"{"verdict":"approve","notes":"ok"}"#.into(), 50),
        ]);
        let res = runner.run("add a greet function").await.unwrap();
        assert_eq!(res.verdict, "approve");
        assert!(res.diff.contains("greet"));
        assert_eq!(res.worker_tokens, 100);
        assert_eq!(res.critic_tokens, 50);
        assert!(res.artifacts.iter().any(|p| p.ends_with(".md")));
        assert!(res.artifacts.iter().any(|p| p.ends_with(".diff")));
    }

    #[tokio::test]
    async fn redirect_then_approve_runs_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture_root(tmp.path());
        let runner = CriticRunner {
            project_root: root,
            worker_model: "w".into(),
            critic_model: "c".into(),
            proxy_url: None,
            api_key: "fake".into(),
            fake: None,
        }
        .with_fake_responses(vec![
            ("<plan>1. wrong</plan><diff>bad</diff>".into(), 10),
            (r#"{"verdict":"redirect","notes":"wrong file"}"#.into(), 5),
            ("<plan>1. fixed</plan><diff>better</diff>".into(), 12),
            (r#"{"verdict":"approve","notes":"ok"}"#.into(), 6),
        ]);
        let res = runner.run("do something").await.unwrap();
        assert_eq!(res.verdict, "approve");
        assert!(res.plan.contains("fixed"));
        assert!(res.diff.contains("better"));
        assert_eq!(res.worker_tokens, 22);
        assert_eq!(res.critic_tokens, 11);
    }

    #[tokio::test]
    async fn empty_diff_skips_diff_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let root = fixture_root(tmp.path());
        let runner = CriticRunner {
            project_root: root,
            worker_model: "w".into(),
            critic_model: "c".into(),
            proxy_url: None,
            api_key: "fake".into(),
            fake: None,
        }
        .with_fake_responses(vec![
            ("<plan>1. explore</plan><diff></diff>".into(), 1),
            (r#"{"verdict":"approve","notes":"nothing to do"}"#.into(), 1),
        ]);
        let res = runner.run("explore").await.unwrap();
        assert!(res.artifacts.iter().any(|p| p.ends_with(".md")));
        assert!(!res.artifacts.iter().any(|p| p.ends_with(".diff")));
    }
}

