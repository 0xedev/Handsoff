//! Headless agent spawn for failover.
//!
//! `headless_spawn()` resolves the right binary and CLI args via the
//! `Adapter` trait (see `handoff_adapters::for_kind_str`), verifies the
//! binary is on PATH, then launches it with the proxy env and tee logging.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use anyhow::{anyhow, Result};
use handoff_common::home_dir;
use tokio::process::{Child, Command};

/// Returns true if `bin` resolves to an executable file anywhere in `$PATH`.
fn binary_on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path_var| std::env::split_paths(&path_var).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

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

/// Spawn an agent in one-shot mode with `prompt` as input. Stdout/stderr
/// are tee'd to `~/.handoff/tee/agent-<kind>-<ts>.log`. Returns
/// `Ok(None)` for agents (e.g. cursor) that have no headless form.
pub async fn headless_spawn(
    agent_id: i64,
    kind: &str,
    project_root: &Path,
    prompt: &str,
    proxy_url: &str,
    use_worktree: bool,
) -> Result<Option<Child>> {
    let mut effective_root = project_root.to_path_buf();
    if use_worktree {
        effective_root = crate::worktree::create(project_root, agent_id)?;
    }

    // Unknown kinds (e.g. "simulated" from tests) return Ok(None) gracefully.
    let Some(adapter) = handoff_adapters::for_kind_str(kind) else {
        return Ok(None);
    };
    let args = adapter.headless_args(prompt);

    let Some(args) = args else {
        return Ok(None);
    };

    let bin = adapter.binaries()[0];

    // Verify the binary is on PATH before attempting spawn.
    if !binary_on_path(bin) {
        anyhow::bail!(
            "agent binary '{}' not found on PATH; install it before using handoff spawn",
            bin
        );
    }

    let log_path = handoff_common::tee::tee_path(agent_id);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = handoff_common::tee::rotate_if_needed(&log_path);

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| anyhow!("opening tee log {}: {e}", log_path.display()))?;
    let log_stderr = log.try_clone()?;

    let mut cmd = Command::new(bin);
    cmd.args(&args)
        .current_dir(effective_root)
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
    fn proxy_env_sets_all_keys() {
        let env = proxy_env("http://127.0.0.1:8080");
        assert!(env.contains_key("HTTPS_PROXY"));
        assert!(env.contains_key("http_proxy"));
    }

    #[test]
    fn binary_on_path_finds_sh() {
        // `sh` is present on every POSIX system.
        assert!(binary_on_path("sh"), "expected 'sh' to be on PATH");
    }

    #[test]
    fn binary_on_path_rejects_nonexistent() {
        assert!(!binary_on_path("__handoff_no_such_binary_xyz__"));
    }
}
