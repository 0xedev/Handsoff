use anyhow::Result;
use std::path::Path;

/// Write Claude Code PreToolUse hook config
pub fn install_claude(daemon_url: &str, settings_path: &Path) -> Result<()> {
    let settings_str = std::fs::read_to_string(settings_path).unwrap_or_else(|_| "{}".to_string());
    let mut settings: serde_json::Value = serde_json::from_str(&settings_str)?;

    let hook_cmd = format!(
        "curl -s -X POST {}/hook -H 'Content-Type: application/json' -d '{{\"agent_pid\": $CLAUDE_PID, \"tool_name\": \"$TOOL_NAME\"}}'",
        daemon_url
    );

    settings["hooks"]["PreToolUse"] = serde_json::json!([{"command": hook_cmd}]);
    
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!("Hook installed in {}", settings_path.display());
    Ok(())
}

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
