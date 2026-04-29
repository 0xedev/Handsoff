use std::path::{Path, PathBuf};

/// `~/.handoff/` (overridable via `HANDOFF_HOME`).
pub fn home_dir() -> PathBuf {
    if let Ok(p) = std::env::var("HANDOFF_HOME") {
        let p = PathBuf::from(p);
        let _ = std::fs::create_dir_all(&p);
        return p;
    }
    let base = dirs::home_dir().expect("no home dir").join(".handoff");
    let _ = std::fs::create_dir_all(&base);
    base
}

pub fn db_path() -> PathBuf {
    home_dir().join("state.db")
}

pub fn daemon_pidfile() -> PathBuf {
    home_dir().join("daemon.pid")
}

pub fn proxy_pidfile() -> PathBuf {
    home_dir().join("proxy.pid")
}

pub fn tee_dir() -> PathBuf {
    let p = home_dir().join("tee");
    let _ = std::fs::create_dir_all(&p);
    p
}

/// `<XDG_CONFIG_HOME>/handoff/config.toml` for user-level defaults.
pub fn xdg_config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().expect("no home dir").join(".config"));
    base.join("handoff").join("config.toml")
}

/// Project-scoped `.handoff/` directory.
pub fn project_dir(root: &Path) -> PathBuf {
    root.join(".handoff")
}
