use std::path::{Path, PathBuf};

/// Read the last user+assistant exchange from Claude Code's JSONL session files
pub fn claude_conversation_tail(_project_root: &Path) -> Option<String> {
    // Claude stores sessions in ~/.claude/projects/<hash>/*.jsonl
    // Hash is SHA1-like of the project root path
    let home = dirs::home_dir()?;
    let projects_dir = home.join(".claude/projects");
    if !projects_dir.exists() {
        return None;
    }

    // Find the most recently modified JSONL in any project dir
    let latest_jsonl = find_latest_jsonl(&projects_dir)?;
    let content = std::fs::read_to_string(&latest_jsonl).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    
    // Take last 20 lines — enough for the last exchange
    let count = lines.len();
    let start = count.saturating_sub(20);
    let tail = &lines[start..];
    let tail_str = tail.join("\n");
    
    Some(format!("<!-- last ~20 lines of {} -->\n{}", latest_jsonl.display(), tail_str))
}

fn find_latest_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut latest: Option<(PathBuf, std::time::SystemTime)> = None;
    
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Ok(sub_entries) = std::fs::read_dir(entry.path()) {
                    for sub_entry in sub_entries.flatten() {
                        let path = sub_entry.path();
                        if path.extension().map(|x| x == "jsonl").unwrap_or(false) {
                            if let Ok(meta) = path.metadata() {
                                if let Ok(modified) = meta.modified() {
                                    if latest.is_none() || modified > latest.as_ref().unwrap().1 {
                                        latest = Some((path, modified));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    latest.map(|(p, _)| p)
}
