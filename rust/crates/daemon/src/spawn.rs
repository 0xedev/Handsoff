//! Headless agent spawn for failover.
//!
//! Each agent has a different one-shot CLI form:
//!     claude:  claude -p "<prompt>"
//!     codex:   codex exec "<prompt>"
//!     copilot: gh copilot suggest "<prompt>"
//!     cursor:  not supported (IDE-only)

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use anyhow::{anyhow, Result};
use chrono::Utc;
use handoff_common::{home_dir, tee_dir};
use tokio::process::{Child, Command};

pub fn proxy_env(proxy_url: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("HTTP_PROXY".into(), proxy_url.into());
    env.insert("HTTPS_PROXY".into(), proxy_url.into());
    env.insert("http_proxy".into(), proxy_url.into());
    env.insert("https_proxy".into(), proxy_url.into());
    let ca = home_dir().join("ca").join("cert.pem");
    if ca.exists() {
        let s = ca.display().to_string();
        env.insert("SSL_CERT_FILE".into(), s.clone());
        env.insert("REQUESTS_CA_BUNDLE".into(), s.clone());
        env.insert("NODE_EXTRA_CA_CERTS".into(), s);
    }
    env
}

fn argv_for(kind: &str, prompt: &str) -> Option<Vec<String>> {
    match kind {
        "claude" => Some(vec!["claude".into(), "-p".into(), prompt.into()]),
        "codex" => Some(vec!["codex".into(), "exec".into(), prompt.into()]),
        "copilot" => Some(vec![
            "gh".into(),
            "copilot".into(),
            "suggest".into(),
            prompt.into(),
        ]),
        _ => None,
    }
}

/// Spawn an agent in one-shot mode with `prompt` as input. Stdout/stderr
/// are tee'd to `~/.handoff/tee/agent-<kind>-<ts>.log`. Returns
/// `Ok(None)` for agents (e.g. cursor) that have no headless form.
pub async fn headless_spawn(
    kind: &str,
    project_root: &Path,
    prompt: &str,
    proxy_url: &str,
) -> Result<Option<Child>> {
    let Some(argv) = argv_for(kind, prompt) else {
        return Ok(None);
    };
    let log_path = tee_dir().join(format!(
        "agent-{kind}-{}.log",
        Utc::now().timestamp()
    ));
    let log = std::fs::File::create(&log_path)
        .map_err(|e| anyhow!("creating tee log {}: {e}", log_path.display()))?;
    let log_stderr = log.try_clone()?;

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(project_root)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_stderr));
    for (k, v) in proxy_env(proxy_url) {
        cmd.env(k, v);
    }
    let child = cmd.spawn()?;
    Ok(Some(child))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_known_kinds() {
        assert!(argv_for("claude", "hello").is_some());
        assert!(argv_for("codex", "hello").is_some());
        assert!(argv_for("copilot", "hello").is_some());
        assert!(argv_for("cursor", "hello").is_none());
    }

    #[test]
    fn proxy_env_sets_all_keys() {
        let env = proxy_env("http://127.0.0.1:8080");
        assert!(env.contains_key("HTTPS_PROXY"));
        assert!(env.contains_key("http_proxy"));
    }
}
