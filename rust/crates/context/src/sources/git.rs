//! Git introspection. Shells out — no native git crate as a dep.

use std::path::Path;
use std::process::Command;

use handoff_common::GitHead;

fn run(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(root).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn diff_head(root: &Path) -> Option<String> {
    run(root, &["diff", "HEAD", "--no-color"])
}

pub fn files_touched(root: &Path) -> Option<Vec<String>> {
    let raw = run(root, &["status", "--porcelain"])?;
    let mut out = Vec::new();
    for line in raw.lines() {
        // porcelain format: "XY path" — skip the 3-char prefix
        if line.len() < 4 {
            continue;
        }
        out.push(line[3..].trim().to_string());
    }
    Some(out)
}

pub fn head_info(root: &Path) -> Option<GitHead> {
    let sha = run(root, &["rev-parse", "HEAD"])?.trim().to_string();
    let branch = run(root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let message = run(root, &["log", "-1", "--pretty=%s"])?.trim().to_string();
    if sha.is_empty() {
        return None;
    }
    Some(GitHead {
        sha,
        branch,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo(dir: &Path) {
        Command::new("git").args(["init", "-q"]).current_dir(dir).output().unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t"])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn head_info_after_init() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let h = head_info(tmp.path()).unwrap();
        assert!(!h.sha.is_empty());
        assert_eq!(h.message, "init");
    }

    #[test]
    fn files_touched_lists_modifications() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "world").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "new").unwrap();
        let files = files_touched(tmp.path()).unwrap();
        assert!(files.iter().any(|f| f == "a.txt"));
        assert!(files.iter().any(|f| f == "b.txt"));
    }

    #[test]
    fn diff_head_shows_changes() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join("a.txt"), "world").unwrap();
        let d = diff_head(tmp.path()).unwrap();
        assert!(d.contains("-hello"));
        assert!(d.contains("+world"));
    }

    #[test]
    fn missing_repo_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(head_info(tmp.path()).is_none());
    }
}
