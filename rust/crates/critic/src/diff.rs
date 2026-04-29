use std::io::Write;
use std::path::Path;
use std::process::Stdio;

/// Extract unified diff blocks from a string
pub fn extract_diffs(text: &str) -> Vec<String> {
    let mut diffs = Vec::new();
    let mut current = String::new();
    let mut in_diff = false;

    for line in text.lines() {
        if line.starts_with("diff --git") || line.starts_with("--- a/") {
            if in_diff && !current.is_empty() {
                diffs.push(current.clone());
                current.clear();
            }
            in_diff = true;
        }
        if in_diff {
            current.push_str(line);
            current.push('\n');
        } else if line.starts_with("```diff") {
            in_diff = true;
            current.clear();
        } else if line.starts_with("```") && in_diff {
            in_diff = false;
            if !current.is_empty() {
                diffs.push(current.clone());
                current.clear();
            }
        }
    }
    if in_diff && !current.is_empty() {
        diffs.push(current);
    }
    diffs
}

pub fn apply_check(diff: &str, project_root: &Path) -> bool {
    let child = std::process::Command::new("git")
        .args(["apply", "--check", "-"])
        .current_dir(project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();

    if let Some(mut child) = child {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(diff.as_bytes());
        }
        child.wait().map(|s| s.success()).unwrap_or(false)
    } else {
        false
    }
}
