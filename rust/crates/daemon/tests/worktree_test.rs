// rust/crates/daemon/tests/worktree_test.rs
use handoff_daemon::worktree;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn create_worktree_makes_git_worktree() {
    let tmp = TempDir::new().unwrap();
    // Init a git repo for testing
    Command::new("git")
        .args(["init", tmp.path().to_str().unwrap()])
        .output()
        .expect("git init failed");
    Command::new("git")
        .args([
            "-C",
            tmp.path().to_str().unwrap(),
            "config",
            "user.email",
            "test@example.com",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            tmp.path().to_str().unwrap(),
            "config",
            "user.name",
            "test",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            tmp.path().to_str().unwrap(),
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .output()
        .expect("git commit failed");

    let agent_id = 42i64;
    let result = worktree::create(tmp.path(), agent_id);
    assert!(result.is_ok(), "{:?}", result.err());

    let wt_path = result.unwrap();
    assert!(wt_path.exists());

    // Cleanup
    let _ = worktree::remove(tmp.path(), agent_id);
}
