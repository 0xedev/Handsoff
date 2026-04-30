//! Claude Code `PreToolUse` hook integration.
//!
//! Claude Code hooks receive a JSON payload on **stdin** (not env vars). The
//! hook command is a shell string; stdin is piped in automatically. We write a
//! small shell script to `~/.handoff/hooks/pretooluse.sh` and configure the
//! agent's settings.json to call it.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// The hook script reads the JSON Claude Code sends on stdin, extracts the
/// tool name, uses $PPID as the agent PID (the hook is a child of claude),
/// and POSTs to the handoff daemon.
fn hook_script(daemon_url: &str) -> String {
    format!(
        r#"#!/usr/bin/env sh
# handoff PreToolUse hook
# Claude Code pipes a JSON payload to stdin; $PPID is the claude process.
payload=$(cat)
tool_name=$(printf '%s' "$payload" \
  | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('tool_name','unknown'))" \
  2>/dev/null || echo "unknown")
curl -s -X POST {url}/hook \
  -H 'Content-Type: application/json' \
  -d "{{\"agent_pid\": $PPID, \"tool_name\": \"$tool_name\"}}" \
  >/dev/null 2>&1 || true
"#,
        url = daemon_url
    )
}

fn shell_hook_script() -> String {
    r#"#!/usr/bin/env sh
# handoff shell hook
mkdir -p "$HOME/.handoff"

handoff_should_proxy_command() {
  case "$1" in
    claude|claude\ *|claude-code|claude-code\ *|codex|codex\ *|gh\ copilot|gh\ copilot\ *|gemini|gemini\ *|aider|aider\ *|cline|cline\ *)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

handoff_save_var() {
  eval "if [ \"\${$1+x}\" = x ]; then export HANDOFF_OLD_$1=\"\${$1}\"; export HANDOFF_OLD_${1}_SET=1; else unset HANDOFF_OLD_$1; export HANDOFF_OLD_${1}_SET=0; fi"
}

handoff_restore_var() {
  eval "if [ \"\${HANDOFF_OLD_${1}_SET-0}\" = 1 ]; then export $1=\"\${HANDOFF_OLD_$1}\"; else unset $1; fi; unset HANDOFF_OLD_$1 HANDOFF_OLD_${1}_SET"
}

handoff_enable_proxy() {
  if [ -n "${HANDOFF_PROXY_ACTIVE-}" ]; then
    return
  fi
  handoff_save_var HTTP_PROXY
  handoff_save_var HTTPS_PROXY
  handoff_save_var ALL_PROXY
  handoff_save_var NODE_EXTRA_CA_CERTS
  handoff_save_var REQUESTS_CA_BUNDLE
  handoff_save_var SSL_CERT_FILE
  export HANDOFF_PROXY_ACTIVE=1
  export HTTP_PROXY="${HANDOFF_PROXY_URL:-http://127.0.0.1:8080}"
  export HTTPS_PROXY="${HANDOFF_PROXY_URL:-http://127.0.0.1:8080}"
  export ALL_PROXY="${HANDOFF_PROXY_URL:-http://127.0.0.1:8080}"
  export NODE_EXTRA_CA_CERTS="$HOME/.handoff/ca/cert.pem"
  export REQUESTS_CA_BUNDLE="$HOME/.handoff/ca/cert.pem"
  export SSL_CERT_FILE="$HOME/.handoff/ca/cert.pem"
}

handoff_restore_proxy() {
  if [ -z "${HANDOFF_PROXY_ACTIVE-}" ]; then
    return
  fi
  handoff_restore_var HTTP_PROXY
  handoff_restore_var HTTPS_PROXY
  handoff_restore_var ALL_PROXY
  handoff_restore_var NODE_EXTRA_CA_CERTS
  handoff_restore_var REQUESTS_CA_BUNDLE
  handoff_restore_var SSL_CERT_FILE
  unset HANDOFF_PROXY_ACTIVE
}

handoff_log_command() {
  printf '%s\n' "$1" >> "$HOME/.handoff/cmdlog.txt"
}

handoff_before_command() {
  handoff_log_command "$1"
  if handoff_should_proxy_command "$1"; then
    handoff_enable_proxy
  else
    handoff_restore_proxy
  fi
}

if [ -n "${ZSH_VERSION-}" ]; then
  if command -v add-zsh-hook >/dev/null 2>&1; then
    autoload -Uz add-zsh-hook >/dev/null 2>&1 || true
    add-zsh-hook preexec handoff_before_command
    add-zsh-hook precmd handoff_restore_proxy
  fi
elif [ -n "${BASH_VERSION-}" ]; then
  trap 'handoff_before_command "$BASH_COMMAND"' DEBUG
  PROMPT_COMMAND="handoff_restore_proxy${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
"#
    .to_string()
}

fn ensure_rc_block(rc_path: &Path, block: &str) -> Result<()> {
    let existing = std::fs::read_to_string(rc_path).unwrap_or_default();
    if existing.contains(block) {
        return Ok(());
    }
    if let Some(parent) = rc_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = existing;
    if !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    body.push_str(block);
    body.push('\n');
    std::fs::write(rc_path, body)?;
    Ok(())
}

/// Install the PreToolUse hook for Claude Code.
///
/// Writes a self-contained shell script to `~/.handoff/hooks/pretooluse.sh`
/// and registers it in `settings_path` (typically `~/.claude/settings.json`).
pub fn install_claude(daemon_url: &str, settings_path: &Path) -> Result<()> {
    // Write the hook script
    let hooks_dir = handoff_common::home_dir().join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let script_path = hooks_dir.join("pretooluse.sh");
    std::fs::write(&script_path, hook_script(daemon_url))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // Read / update Claude Code settings.json
    let settings_str = std::fs::read_to_string(settings_path).unwrap_or_else(|_| "{}".to_string());
    let mut settings: serde_json::Value = serde_json::from_str(&settings_str)?;

    // Claude Code hook format: hooks.PreToolUse = [{ command: "..." }]
    let cmd = script_path.to_string_lossy().to_string();
    settings["hooks"]["PreToolUse"] = serde_json::json!([{"command": cmd}]);

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&settings)?)?;

    println!("Hook script: {}", script_path.display());
    println!("Hook registered in {}", settings_path.display());
    Ok(())
}

/// Install the shell command-log hook used by the snapshot engine.
pub fn install_shell() -> Result<PathBuf> {
    let hooks_dir = handoff_common::home_dir().join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let script_path = hooks_dir.join("shell.sh");
    std::fs::write(&script_path, shell_hook_script())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let block = format!(
        r#"# handoff begin
if [ -f "{script}" ]; then
  . "{script}"
fi
# handoff end"#,
        script = script_path.display()
    );
    let zshrc = dirs::home_dir().unwrap().join(".zshrc");
    let bashrc = dirs::home_dir().unwrap().join(".bashrc");
    let _ = ensure_rc_block(&zshrc, &block);
    let _ = ensure_rc_block(&bashrc, &block);

    Ok(script_path)
}

/// Remove the PreToolUse hook from Claude Code settings.
pub fn uninstall_claude(settings_path: &Path) -> Result<()> {
    if !settings_path.exists() {
        return Ok(());
    }
    let settings_str = std::fs::read_to_string(settings_path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&settings_str)?;
    if let Some(hooks) = settings.get_mut("hooks") {
        hooks.as_object_mut().map(|h| h.remove("PreToolUse"));
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!("Hook removed from {}", settings_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_script_reads_stdin_not_env_vars() {
        let script = hook_script("http://127.0.0.1:7879");
        // Must use stdin (cat), not undefined env vars
        assert!(script.contains("payload=$(cat)"));
        assert!(script.contains("$PPID"));
        assert!(!script.contains("$CLAUDE_PID"));
        assert!(!script.contains("$TOOL_NAME"));
    }

    #[test]
    fn hook_script_contains_correct_url() {
        let s = hook_script("http://localhost:9999");
        assert!(s.contains("http://localhost:9999/hook"));
        assert!(s.contains("python3"));
    }

    #[test]
    fn shell_hook_proxies_known_agent_commands() {
        let script = shell_hook_script();
        assert!(script.contains("handoff_should_proxy_command"));
        assert!(script.contains("claude|claude\\ *"));
        assert!(script.contains("codex|codex\\ *"));
        assert!(script.contains("gh\\ copilot"));
        assert!(script.contains("HTTPS_PROXY"));
        assert!(script.contains("NODE_EXTRA_CA_CERTS"));
    }
}
