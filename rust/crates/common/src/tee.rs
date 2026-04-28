use std::io::{self};
use std::path::{Path, PathBuf};

const MAX_SIZE_BYTES: u64 = 10 * 1024 * 1024; // 10MB
const MAX_ROTATIONS: u32 = 5;

pub fn tee_path(agent_id: i64) -> PathBuf {
    crate::paths::home_dir()
        .join("tee")
        .join(format!("agent-{}.log", agent_id))
}

pub fn rotate_if_needed(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let size = std::fs::metadata(path)?.len();
    if size < MAX_SIZE_BYTES {
        return Ok(());
    }

    // Shift old rotations: .5 → delete, .4 → .5, ... .1 → .2
    for i in (1..MAX_ROTATIONS).rev() {
        let from = PathBuf::from(format!("{}.{}", path.display(), i));
        let to = PathBuf::from(format!("{}.{}", path.display(), i + 1));
        if from.exists() {
            if i + 1 >= MAX_ROTATIONS {
                let _ = std::fs::remove_file(&to);
            } else {
                std::fs::rename(&from, &to)?;
            }
        }
    }
    let rotated = PathBuf::from(format!("{}.1", path.display()));
    std::fs::rename(path, rotated)?;
    Ok(())
}

pub fn tail(path: &Path, lines: usize) -> io::Result<String> {
    let content = std::fs::read_to_string(path)?;
    let last: Vec<&str> = content.lines().rev().take(lines).collect();
    Ok(last.into_iter().rev().collect::<Vec<_>>().join("\n"))
}
