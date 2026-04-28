use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

fn worktree_dir(agent_id: i64) -> PathBuf {
    handoff_common::home_dir()
        .join("worktrees")
        .join(format!("agent-{}", agent_id))
}

pub fn create(project_root: &Path, agent_id: i64) -> Result<PathBuf> {
    let dest = worktree_dir(agent_id);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Ensure we don't have a stale worktree
    let _ = remove(project_root, agent_id);

    let status = Command::new("git")
        .args(["worktree", "add", dest.to_str().unwrap(), "HEAD"])
        .current_dir(project_root)
        .status()?;

    anyhow::ensure!(status.success(), "git worktree add failed");
    Ok(dest)
}

pub fn diff(agent_id: i64) -> Result<String> {
    let wt = worktree_dir(agent_id);
    let output = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(&wt)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn save_patch(agent_id: i64) -> Result<PathBuf> {
    let patch_dir = handoff_common::home_dir().join("diffs");
    std::fs::create_dir_all(&patch_dir)?;
    let patch_path = patch_dir.join(format!("agent-{}.patch", agent_id));
    let diff_output = diff(agent_id)?;
    std::fs::write(&patch_path, diff_output)?;
    Ok(patch_path)
}

pub fn remove(project_root: &Path, agent_id: i64) -> Result<()> {
    let wt = worktree_dir(agent_id);
    if !wt.exists() {
        return Ok(());
    }
    
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force", wt.to_str().unwrap()])
        .current_dir(project_root)
        .status()?;
    
    // Prune stale worktrees just in case
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(project_root)
        .status()?;

    Ok(())
}

pub fn list(project_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()?;
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in stdout.lines() {
        if let Some(path_str) = line.strip_prefix("worktree ") {
            out.push(PathBuf::from(path_str));
        }
    }
    Ok(out)
}
