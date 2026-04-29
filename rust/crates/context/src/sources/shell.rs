//! Best-effort recent-commands source.
//!
//! Strategy:
//!   1. If `<project>/.handoff/scratch/cmdlog.txt` exists (written by an
//!      optional shell hook the user installs), use the last N lines.
//!   2. Otherwise fall back to `~/.bash_history` / `~/.zsh_history`.
//!   3. Otherwise return an empty list.
//!
//! We never include arguments that look like secrets (heuristic: AWS_/SECRET/TOKEN/KEY).

use std::path::{Path, PathBuf};

const SECRET_NEEDLES: &[&str] = &[
    "AWS_",
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "API_KEY",
    "PRIVATE_KEY",
];

fn looks_secret(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    SECRET_NEEDLES.iter().any(|n| upper.contains(n))
}

fn last_n(lines: impl IntoIterator<Item = String>, n: usize) -> Vec<String> {
    let v: Vec<String> = lines.into_iter().filter(|l| !l.trim().is_empty()).collect();
    let start = v.len().saturating_sub(n);
    v[start..]
        .iter()
        .map(|l| {
            if looks_secret(l) {
                "[redacted]".to_string()
            } else {
                l.clone()
            }
        })
        .collect()
}

fn read_file_lines(p: &Path) -> Option<Vec<String>> {
    let body = std::fs::read_to_string(p).ok()?;
    Some(body.lines().map(|s| s.to_string()).collect())
}

pub fn recent_commands(project_root: &Path, n: usize) -> Vec<String> {
    let cmdlog = project_root
        .join(".handoff")
        .join("scratch")
        .join("cmdlog.txt");
    if let Some(lines) = read_file_lines(&cmdlog) {
        return last_n(lines, n);
    }

    let global_cmdlog = handoff_common::home_dir().join("cmdlog.txt");
    if let Some(lines) = read_file_lines(&global_cmdlog) {
        return last_n(lines, n);
    }

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };
    for hist in [".zsh_history", ".bash_history"] {
        let p: PathBuf = home.join(hist);
        if let Some(lines) = read_file_lines(&p) {
            // zsh history lines are like ": 1700000000:0;cmd args"
            let cleaned: Vec<String> = lines
                .into_iter()
                .map(|l| {
                    if let Some(idx) = l.find(';') {
                        l[idx + 1..].to_string()
                    } else {
                        l
                    }
                })
                .collect();
            return last_n(cleaned, n);
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_lines() {
        let lines = vec![
            "git status".to_string(),
            "export API_KEY=abc".to_string(),
            "ls".to_string(),
        ];
        let out = last_n(lines, 10);
        assert_eq!(out.len(), 3);
        assert!(out.iter().any(|l| l == "[redacted]"));
    }

    #[test]
    fn cmdlog_takes_precedence() {
        let t = tempfile::tempdir().unwrap();
        let proj = t.path();
        std::fs::create_dir_all(proj.join(".handoff/scratch")).unwrap();
        std::fs::write(proj.join(".handoff/scratch/cmdlog.txt"), "git status\nls\n").unwrap();
        let out = recent_commands(proj, 10);
        assert_eq!(out, vec!["git status".to_string(), "ls".to_string()]);
    }
}
