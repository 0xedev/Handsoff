//! mtime-polling file watcher used by `handoff critic watch`.
//!
//! No new dependencies — calls `git ls-files` when the directory is a git
//! repo, falls back to a `WalkDir`-style walk otherwise. Same debounce
//! semantics as the Python `WatchLoop`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Best-effort tracked-file enumeration.
pub fn list_tracked_files(root: &Path) -> Vec<PathBuf> {
    let res = Command::new("git")
        .arg("ls-files")
        .current_dir(root)
        .output();
    if let Ok(out) = res {
        if out.status.success() && !out.stdout.is_empty() {
            return String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| root.join(l))
                .collect();
        }
    }
    fallback_walk(root)
}

fn fallback_walk(root: &Path) -> Vec<PathBuf> {
    fn skip(name: &str) -> bool {
        matches!(
            name,
            ".git" | ".handoff" | "node_modules" | "__pycache__" | ".venv" | "venv" | "target"
        )
    }
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name();
            let name_s = name.to_string_lossy();
            if skip(&name_s) {
                continue;
            }
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                out.push(p);
            }
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct WatchTick {
    pub changed: HashSet<PathBuf>,
    pub fired: bool,
}

/// Polling watcher with a debounce window. `interval` and `debounce` are
/// expressed in seconds. The caller drives the loop via [`tick`].
pub struct WatchLoop {
    root: PathBuf,
    interval: f64,
    debounce: f64,
    mtimes: HashMap<PathBuf, SystemTime>,
    pending: HashSet<PathBuf>,
    last_change: Option<f64>,
}

impl WatchLoop {
    pub fn new(root: &Path, interval_secs: f64, debounce_secs: f64) -> Self {
        let mut me = Self {
            root: root.to_path_buf(),
            interval: interval_secs,
            debounce: debounce_secs,
            mtimes: HashMap::new(),
            pending: HashSet::new(),
            last_change: None,
        };
        me.snapshot();
        me
    }

    pub fn interval_secs(&self) -> f64 {
        self.interval
    }

    fn snapshot(&mut self) {
        for p in self.scan() {
            if let Ok(m) = p.metadata().and_then(|m| m.modified()) {
                self.mtimes.insert(p, m);
            }
        }
    }

    fn scan(&self) -> Vec<PathBuf> {
        list_tracked_files(&self.root)
    }

    /// One poll iteration. Pass an explicit `now` (epoch seconds) for
    /// deterministic tests; in production, callers use `tick_now()`.
    pub fn tick(&mut self, now: f64) -> WatchTick {
        let mut changed = HashSet::new();
        for p in self.scan() {
            let Ok(m) = p.metadata().and_then(|m| m.modified()) else { continue };
            let prev = self.mtimes.get(&p).copied();
            self.mtimes.insert(p.clone(), m);
            if let Some(prev_m) = prev {
                if m > prev_m {
                    changed.insert(p);
                }
            }
        }
        if !changed.is_empty() {
            self.pending.extend(changed.iter().cloned());
            self.last_change = Some(now);
        }
        let mut fired = false;
        if let Some(last) = self.last_change {
            if !self.pending.is_empty() && (now - last) >= self.debounce {
                fired = true;
                self.last_change = None;
                self.pending.clear();
            }
        }
        WatchTick { changed, fired }
    }

    /// Convenience wrapper around `tick` with the current wall-clock time.
    pub fn tick_now(&mut self) -> WatchTick {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.tick(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::{Duration, UNIX_EPOCH};

    fn touch(p: &Path, secs: u64) {
        File::create(p).unwrap();
        let t = UNIX_EPOCH + Duration::from_secs(secs);
        filetime::set_file_mtime(p, filetime::FileTime::from_system_time(t)).unwrap();
    }

    #[test]
    fn fallback_skips_handoff_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".handoff")).unwrap();
        File::create(tmp.path().join(".handoff").join("brain.md")).unwrap();
        File::create(tmp.path().join("src.rs")).unwrap();
        let files: Vec<_> = fallback_walk(tmp.path())
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(files.contains(&"src.rs".to_string()));
        assert!(!files.contains(&"brain.md".to_string()));
    }

    #[test]
    fn debounce_fires_after_window() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("x.rs");
        let base: u64 = 1_000_000_000;
        touch(&target, base);

        let mut loop_ = WatchLoop::new(tmp.path(), 0.01, 1.0);

        // No change: no fire
        let tick = loop_.tick(base as f64 + 1.0);
        assert!(!tick.fired);

        // mtime bumped, but debounce window not yet elapsed
        touch(&target, base + 5);
        let tick = loop_.tick(base as f64 + 5.1);
        assert!(tick.changed.contains(&target));
        assert!(!tick.fired);

        // Another change resets the window
        touch(&target, base + 6);
        let tick = loop_.tick(base as f64 + 6.1);
        assert!(!tick.fired);

        // Now well past debounce
        let tick = loop_.tick(base as f64 + 10.0);
        assert!(tick.fired);
    }

    #[test]
    fn no_changes_no_fire() {
        let tmp = tempfile::tempdir().unwrap();
        File::create(tmp.path().join("x")).unwrap();
        let mut loop_ = WatchLoop::new(tmp.path(), 0.01, 0.5);
        for now in &[1.0_f64, 2.0, 3.0, 4.0] {
            assert!(!loop_.tick(*now).fired);
        }
    }
}
