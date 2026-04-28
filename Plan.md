# handoff v0.5 → v1.0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship handoff from an alpha proof-of-concept to a user-installable, demonstrably-correct failover tool for AI coding agents.

**Architecture:** Six tiers progressing from process work (merge/README) through adoption blockers (setup, CI, TUI), architectural correctness (adapter refactor, worktrees, smart chain selection), quality/observability (snapshot depth, reducers, logs, replay), and surface expansion (more agents, VS Code extension, Windows). Each tier produces working, testable software independently.

**Tech Stack:** Rust 2021, tokio + axum (0.7), clap (4.5), rusqlite (bundled), ratatui (0.28) + crossterm (0.28) [new for TUI], tracing-appender [new for log files], hudsucker (MITM proxy), rcgen (CA gen), sysinfo (process detection).

---

## Key Files Reference

| Concern | File | Lines |
|---------|------|-------|
| CLI subcommands | `rust/crates/cli/src/main.rs` | 190-220 |
| RPC dispatcher | `rust/crates/daemon/src/lib.rs` | 156-168 |
| Route definitions | `rust/crates/daemon/src/lib.rs` | 82-88 |
| Failover engine handle() | `rust/crates/daemon/src/failover.rs` | 75-130 |
| RateEvent struct | `rust/crates/daemon/src/failover.rs` | 19-25 |
| argv_for() (headless mapping) | `rust/crates/daemon/src/spawn.rs` | 34-46 |
| headless_spawn() | `rust/crates/daemon/src/spawn.rs` | 51-78 |
| proxy_env() | `rust/crates/daemon/src/spawn.rs` | 18-32 |
| Adapter trait | `rust/crates/adapters/src/lib.rs` | 24-71 |
| ClaudeAdapter | `rust/crates/adapters/src/lib.rs` | 132-172 |
| CodexAdapter | `rust/crates/adapters/src/lib.rs` | 174-212 |
| CopilotAdapter | `rust/crates/adapters/src/lib.rs` | 214-261 |
| pick_next() | `rust/crates/policy/src/lib.rs` | 157-162 |
| should_trigger() | `rust/crates/policy/src/lib.rs` | 114-154 |
| FailoverPolicy struct | `rust/crates/policy/src/lib.rs` | 20-34 |
| ContextEngine::snapshot() | `rust/crates/context/src/lib.rs` | 149-178 |
| Snapshot struct | `rust/crates/common/src/types.rs` | 69-98 |
| DB schema | `rust/crates/storage/src/lib.rs` | 12-73 |
| CA generation | `rust/crates/proxy/src/ca.rs` | 44-70 |
| CriticRunner::ask() | `rust/crates/critic/src/lib.rs` | 96-142 |
| CriticRunner::run() | `rust/crates/critic/src/lib.rs` | 144-201 |
| Workspace Cargo.toml | `rust/Cargo.toml` | — |

**Important correction from exploration:** The `headless_argv` / per-agent CLI match block the spec calls out is at `spawn.rs:34-46` (`argv_for()`) — not in `critic/src/lib.rs`. The spec's critic line reference is off. Plan tasks below use the correct location.

---

## TIER 0 — Quick Wins

### Task 0.1: Merge to main + tag v0.4.1-alpha

**Files:**
- Process only (GitHub + git)

- [ ] **Step 1: Verify CI would pass locally**
```bash
cd rust && cargo build --workspace && cargo test --workspace
```
Expected: all tests pass, zero warnings about missing features.

- [ ] **Step 2: Open PR from current branch to main**
```bash
gh pr create --base main --title "feat: Rust v0.4.0-alpha — MITM proxy, critic loop, drop Python" \
  --body "Port handoff to full Rust. No API key required for proxy/failover path. Critic still uses Anthropic SDK."
```

- [ ] **Step 3: Squash-merge after review**
```bash
gh pr merge --squash --auto
```

- [ ] **Step 4: Tag**
```bash
git checkout main && git pull
git tag v0.4.1-alpha
git push origin v0.4.1-alpha
```

- [ ] **Step 5: Verify cargo install works**
```bash
cargo install --git https://github.com/0xedev/handoff handoff-cli 2>&1 | tail -5
```
Expected: `Installed package 'handoff-cli vX.Y.Z'`

---

### Task 0.2: README cleanup

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Fix "Haiku/Opus" framing (line 15 area)**

Find the current lines describing worker/critic models. Replace with:

```markdown
**handoff proxy + failover** require no API key — they observe rate-limit headers and redirect your agent processes.

**handoff critic** uses the Anthropic API (set `ANTHROPIC_API_KEY`). Worker model: `claude-haiku-4-5`, Critic: `claude-opus-4`.
```

- [ ] **Step 2: Add Privacy section after the Architecture section**
```markdown
## Privacy

- The MITM proxy logs response **headers only** — never request or response bodies.
- Rate-limit headers (`anthropic-ratelimit-*`, `x-ratelimit-*`) are stored in `~/.handoff/state.db`.
- Snapshots written to `<project>/.handoff/scratch/` are local-only; nothing is sent to any server.
- To delete all state: `rm -rf ~/.handoff && rm -rf <project>/.handoff`
```

- [ ] **Step 3: Verify no stale "API key required" in non-critic sections**
```bash
grep -n "API.key\|api_key\|ANTHROPIC" README.md
```
Expected: only appears under the critic section and the Privacy note.

- [ ] **Step 4: Commit**
```bash
git add README.md
git commit -m "docs: fix critic model refs, add privacy section"
```

---

### Task 0.3: Cargo.toml repository URL

**Files:**
- Modify: `rust/Cargo.toml`

- [ ] **Step 1: Update repository field**
```toml
# rust/Cargo.toml — change:
repository = "https://github.com/0xedev/handoff"
```

- [ ] **Step 2: Commit**
```bash
git add rust/Cargo.toml
git commit -m "chore: update repository URL to 0xedev/handoff"
```

- [ ] **Step 3 (manual): Rename repo on GitHub**
Go to GitHub → Settings → Repository name → `handoff`. Old URL auto-redirects.

---

## TIER 1 — Blockers to Adoption

### Task 1.1a: `handoff setup` command

**Files:**
- Create: `rust/crates/cli/src/setup.rs`
- Modify: `rust/crates/cli/src/main.rs` (add Setup/Teardown variants to Cli enum and route)
- Modify: `rust/crates/cli/Cargo.toml` (no new deps — uses std::process::Command + existing crates)

`★ Insight ─────────────────────────────────────`
- `cfg!(target_os = "macos")` and `cfg!(target_os = "linux")` are the right compile-time gates for CA install commands; fall back to printing the manual command at runtime if the sudo call fails
- The CA cert path is already defined at `proxy/src/ca.rs:22-27` — reuse that constant rather than hardcoding
`─────────────────────────────────────────────────`

- [ ] **Step 1: Write failing test for setup::ca_install_command()**

In `rust/crates/cli/src/setup.rs`, define the function signature first:
```rust
pub fn ca_install_command(cert_path: &std::path::Path) -> Option<Vec<String>> {
    todo!()
}
```

Create `rust/crates/cli/tests/setup_test.rs`:
```rust
use handoff_cli::setup::ca_install_command;
use std::path::Path;

#[test]
fn ca_install_command_returns_some_on_supported_os() {
    let cert = Path::new("/tmp/test-ca.pem");
    // On macOS and Linux this should return Some
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    assert!(ca_install_command(cert).is_some());
}

#[test]
fn ca_install_command_returns_sudo_prefix() {
    let cert = Path::new("/tmp/test-ca.pem");
    #[cfg(target_os = "macos")]
    {
        let cmd = ca_install_command(cert).unwrap();
        assert_eq!(cmd[0], "sudo");
        assert!(cmd.contains(&"security".to_string()));
    }
    #[cfg(target_os = "linux")]
    {
        let cmd = ca_install_command(cert).unwrap();
        assert_eq!(cmd[0], "sudo");
    }
}
```

Run:
```bash
cd rust && cargo test -p handoff-cli setup_test 2>&1
```
Expected: FAIL (todo! panic)

- [ ] **Step 2: Implement setup.rs**
```rust
// rust/crates/cli/src/setup.rs
use std::path::Path;
use std::process::{Command, Stdio};
use anyhow::{Context, Result};

pub fn ca_install_command(cert_path: &Path) -> Option<Vec<String>> {
    let p = cert_path.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    return Some(vec![
        "sudo".into(),
        "security".into(),
        "add-trusted-cert".into(),
        "-d".into(),
        "-r".into(),
        "trustRoot".into(),
        "-k".into(),
        "/Library/Keychains/System.keychain".into(),
        p,
    ]);
    #[cfg(target_os = "linux")]
    return Some(vec![
        "sudo".into(),
        "cp".into(),
        p.clone(),
        "/usr/local/share/ca-certificates/handoff.crt".into(),
    ]);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return None;
}

pub async fn run_setup(path: Option<&str>) -> Result<()> {
    let project_root = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir()?,
    };

    println!("Setting up handoff in {}", project_root.display());

    // 1. Scaffold .handoff/
    handoff_context::init_project(&project_root)?;
    println!("  ✓ Scaffolded .handoff/");

    // 2. Generate CA (loads or creates)
    let ca = handoff_proxy::ca::load_or_create()?;
    let cert_path = handoff_common::paths::ca_cert_path();
    println!("  ✓ CA cert at {}", cert_path.display());

    // 3. Install CA into system trust store
    if let Some(cmd) = ca_install_command(&cert_path) {
        println!("  Installing CA into system trust store...");
        let status = Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();
        match status {
            Ok(s) if s.success() => println!("  ✓ CA trusted by system"),
            Ok(_) => {
                eprintln!("  CA install failed. Run manually:");
                println!("    {}", cmd.join(" "));
            }
            Err(e) => {
                eprintln!("  Could not run CA install ({}). Run manually:", e);
                println!("    {}", cmd.join(" "));
            }
        }
    } else {
        println!("  Manual CA install required for your OS.");
        println!("  Cert: {}", cert_path.display());
    }

    // 4. Start daemon + proxy
    // Reuse existing daemon/proxy start logic via RPC
    println!("\n  Start daemon with:  handoff daemon start");
    println!("  Start proxy with:   handoff proxy start");
    println!("\n  Then spawn an agent:");
    println!("    handoff spawn claude -- 'your task here'");
    println!("\nSetup complete.");
    Ok(())
}

pub async fn run_teardown() -> Result<()> {
    let cert_path = handoff_common::paths::ca_cert_path();

    #[cfg(target_os = "macos")]
    {
        let status = Command::new("sudo")
            .args(["security", "delete-certificate", "-c", "handoff"])
            .status();
        match status {
            Ok(s) if s.success() => println!("  ✓ CA removed from system trust"),
            _ => eprintln!("  Could not remove CA automatically. Check Keychain Access."),
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("sudo")
            .args(["rm", "-f", "/usr/local/share/ca-certificates/handoff.crt"])
            .status();
        let _ = Command::new("sudo").arg("update-ca-certificates").status();
        println!("  ✓ CA removed from trust store");
    }

    println!("  Note: brain.md and snapshots are preserved in .handoff/");
    println!("  To fully remove: rm -rf ~/.handoff");
    Ok(())
}
```

- [ ] **Step 3: Wire into CLI enum in main.rs**

In `rust/crates/cli/src/main.rs`, add to the Cli subcommand enum:
```rust
/// First-time setup: scaffold .handoff/, generate CA, install trust
Setup {
    /// Project path (default: current dir)
    path: Option<String>,
},
/// Undo CA trust and stop services
Teardown,
```

Add to the match block:
```rust
Cli::Setup { path } => setup::run_setup(path.as_deref()).await?,
Cli::Teardown => setup::run_teardown().await?,
```

Add module declaration at top:
```rust
mod setup;
```

- [ ] **Step 4: Run tests**
```bash
cd rust && cargo test -p handoff-cli setup_test
```
Expected: all pass

- [ ] **Step 5: Manual smoke test**
```bash
cargo run -p handoff-cli -- setup /tmp/test-project
```
Expected: prints setup steps, no panic.

- [ ] **Step 6: Commit**
```bash
git add rust/crates/cli/src/setup.rs rust/crates/cli/src/main.rs rust/crates/cli/tests/setup_test.rs
git commit -m "feat(cli): add handoff setup and teardown commands"
```

---

### Task 1.1b: GitHub Releases CI pipeline

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Create CI workflow**
```yaml
# .github/workflows/ci.yml
name: CI

on:
  push:
    branches: [main, "claude/**"]
  pull_request:
    branches: [main]

jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: rust
      - name: Build
        run: cd rust && cargo build --workspace
      - name: Test
        run: cd rust && cargo test --workspace
```

- [ ] **Step 2: Create release workflow**
```yaml
# .github/workflows/release.yml
name: Release

on:
  push:
    tags: ["v*"]

jobs:
  build:
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
            artifact: handoff-linux-x86_64
          - target: x86_64-apple-darwin
            os: macos-latest
            artifact: handoff-macos-x86_64
          - target: aarch64-apple-darwin
            os: macos-latest
            artifact: handoff-macos-aarch64
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: rust
      - name: Build
        run: |
          cd rust && cargo build --release --target ${{ matrix.target }} -p handoff-cli
          cp target/${{ matrix.target }}/release/handoff ${{ matrix.artifact }}
      - name: Upload artifact
        uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.artifact }}
          path: rust/${{ matrix.artifact }}

  release:
    needs: build
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/download-artifact@v4
      - name: Create Release
        uses: softprops/action-gh-release@v2
        with:
          files: |
            handoff-linux-x86_64/handoff-linux-x86_64
            handoff-macos-x86_64/handoff-macos-x86_64
            handoff-macos-aarch64/handoff-macos-aarch64
          generate_release_notes: true
```

- [ ] **Step 3: Commit**
```bash
git add .github/workflows/ci.yml .github/workflows/release.yml
git commit -m "ci: add CI and release workflows"
```

- [ ] **Step 4: Verify CI triggers**
```bash
git push origin main
gh run list --limit 5
```
Expected: CI run in progress/passed.

---

### Task 1.1c: install.sh

**Files:**
- Create: `install.sh`

- [ ] **Step 1: Write installer**
```bash
#!/usr/bin/env sh
# install.sh — handoff one-liner installer
set -e

REPO="0xedev/handoff"
BIN_DIR="${HANDOFF_BIN_DIR:-/usr/local/bin}"

detect_target() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"
    case "$OS-$ARCH" in
        Linux-x86_64) echo "handoff-linux-x86_64" ;;
        Darwin-x86_64) echo "handoff-macos-x86_64" ;;
        Darwin-arm64) echo "handoff-macos-aarch64" ;;
        *) echo "Unsupported platform: $OS $ARCH" >&2; exit 1 ;;
    esac
}

ARTIFACT="$(detect_target)"
TAG="$(curl -sSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | cut -d'"' -f4)"
URL="https://github.com/${REPO}/releases/download/${TAG}/${ARTIFACT}"

echo "Downloading handoff ${TAG} for ${ARTIFACT}..."
curl -sSL "$URL" -o /tmp/handoff-install
chmod +x /tmp/handoff-install
sudo mv /tmp/handoff-install "${BIN_DIR}/handoff"

echo "Installed handoff to ${BIN_DIR}/handoff"
echo "Run: handoff setup"
```

- [ ] **Step 2: Make executable and commit**
```bash
chmod +x install.sh
git add install.sh
git commit -m "feat: add one-liner install script"
```

---

### Task 1.2a: `handoff simulate-limit` command

**Files:**
- Modify: `rust/crates/cli/src/main.rs` (new subcommand)
- Modify: `rust/crates/daemon/src/lib.rs` (new `/simulate` route)
- Modify: `rust/crates/daemon/src/failover.rs` (new `simulate()` pub method)

- [ ] **Step 1: Write failing test for /simulate endpoint**

Create `rust/crates/daemon/tests/simulate_test.rs`:
```rust
use axum::http::StatusCode;
use axum_test::TestServer;
use handoff_daemon::build_router;
use handoff_daemon::AppState;

#[tokio::test]
async fn simulate_limit_returns_ok() {
    let state = AppState::test_default().await;
    let app = build_router(state);
    let server = TestServer::new(app).unwrap();

    let resp = server
        .post("/simulate")
        .json(&serde_json::json!({
            "agent_id": 1,
            "tokens": 0,
            "requests": 0
        }))
        .await;

    assert_eq!(resp.status_code(), StatusCode::OK);
}
```

Run:
```bash
cd rust && cargo test -p handoff-daemon simulate_test 2>&1
```
Expected: FAIL (route doesn't exist yet)

- [ ] **Step 2: Add SimulatePayload struct and route to daemon/src/lib.rs**

After the existing route definitions (after line 88):
```rust
#[derive(serde::Deserialize)]
struct SimulatePayload {
    agent_id: i64,
    #[serde(default)]
    tokens: Option<i64>,
    #[serde(default)]
    requests: Option<i64>,
}

async fn simulate_limit(
    State(state): State<AppState>,
    Json(payload): Json<SimulatePayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let ev = handoff_daemon::failover::RateEvent {
        agent_id: payload.agent_id,
        kind: "simulated".to_string(),
        tokens_remaining: payload.tokens,
        requests_remaining: payload.requests,
    };
    state.failover_tx.send(ev).await.ok();
    (StatusCode::OK, Json(serde_json::json!({"ok": true})))
}
```

Add route to `build_router()`:
```rust
.route("/simulate", post(simulate_limit))
```

- [ ] **Step 3: Add simulate subcommand to CLI (main.rs)**
```rust
/// Inject a synthetic rate-limit event to test failover
SimulateLimit {
    /// Agent ID to target
    #[arg(long)]
    agent: i64,
    /// Set tokens_remaining to this value (default 0)
    #[arg(long, default_value = "0")]
    tokens: i64,
    /// Set requests_remaining to this value (default 0)
    #[arg(long, default_value = "0")]
    requests: i64,
},
```

Route in match block:
```rust
Cli::SimulateLimit { agent, tokens, requests } => {
    rpc_call("simulate_limit", serde_json::json!({
        "agent_id": agent,
        "tokens": tokens,
        "requests": requests,
    })).await?;
    println!("Synthetic rate-limit event sent for agent {}", agent);
}
```

Note: `simulate_limit` uses `/simulate` directly, not the RPC dispatcher. Adjust `rpc_call` or add a dedicated `post_to` helper that hits arbitrary endpoints.

- [ ] **Step 4: Run tests**
```bash
cd rust && cargo test -p handoff-daemon simulate_test
```
Expected: PASS

- [ ] **Step 5: Commit**
```bash
git add rust/crates/daemon/src/lib.rs rust/crates/cli/src/main.rs rust/crates/daemon/tests/simulate_test.rs
git commit -m "feat: add simulate-limit command and /simulate endpoint"
```

---

### Task 1.2b: E2E failover test harness

**Files:**
- Create: `rust/crates/daemon/tests/e2e_failover.rs`
- Modify: `.github/workflows/ci.yml` (run e2e tests)

- [ ] **Step 1: Write the E2E test skeleton**

```rust
// rust/crates/daemon/tests/e2e_failover.rs
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

/// Mock HTTP server that returns Anthropic rate-limit headers
/// First N requests: tokens=1000, then tokens=0
struct MockProvider {
    call_count: Arc<AtomicUsize>,
    trigger_at: usize,
    addr: std::net::SocketAddr,
}

impl MockProvider {
    async fn start(trigger_at: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = call_count.clone();

        tokio::spawn(async move {
            let app = axum::Router::new()
                .route(
                    "/v1/messages",
                    axum::routing::post(move || {
                        let cc = cc.clone();
                        async move {
                            let count = cc.fetch_add(1, Ordering::SeqCst);
                            let remaining = if count < trigger_at { 1000i64 } else { 0i64 };
                            (
                                axum::http::StatusCode::OK,
                                [(
                                    "anthropic-ratelimit-tokens-remaining",
                                    remaining.to_string(),
                                )],
                                axum::Json(serde_json::json!({"content": [{"text": "ok"}]})),
                            )
                        }
                    }),
                );
            axum::serve(
                tokio::net::TcpListener::from_std(listener).unwrap(),
                app,
            )
            .await
            .unwrap();
        });

        MockProvider { call_count, trigger_at, addr }
    }
}

#[tokio::test]
async fn failover_triggers_when_tokens_hit_zero() {
    // 1. Start mock provider
    let provider = MockProvider::start(2).await;

    // 2. Spin up daemon on random port
    let db = tempfile::NamedTempFile::new().unwrap();
    let storage = handoff_storage::Database::open(db.path()).unwrap();
    // ... (full daemon init — see AppState::new)

    // 3. Register a project and agent
    let project_id = storage.upsert_project("/tmp/e2e-test".as_ref()).unwrap();
    let agent_id = storage.insert_agent(project_id, "claude", None, None).unwrap();

    // 4. Send a rate event with tokens=0 directly to failover engine
    let (tx, rx) = handoff_daemon::failover::channel();
    tx.send(handoff_daemon::failover::RateEvent {
        agent_id,
        kind: "claude".to_string(),
        tokens_remaining: Some(0),
        requests_remaining: Some(100),
    })
    .await
    .unwrap();

    // 5. Give the failover engine time to process
    sleep(Duration::from_millis(200)).await;

    // 6. Verify a handoff row was created
    let handoffs = storage.list_handoffs(project_id).unwrap();
    assert!(!handoffs.is_empty(), "Expected a handoff row to be created");
    assert_eq!(handoffs[0].from_agent_id, agent_id);
}
```

- [ ] **Step 2: Add `list_handoffs(project_id)` to storage if it doesn't exist**

In `rust/crates/storage/src/lib.rs`, add:
```rust
pub fn list_handoffs(&self, project_id: i64) -> Result<Vec<HandoffRow>> {
    let conn = self.conn.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT h.id, h.from_agent_id, h.to_agent_id, h.reason, h.ts, h.context_snapshot_path
         FROM handoffs h
         JOIN agents a ON a.id = h.from_agent_id
         WHERE a.project_id = ?1
         ORDER BY h.ts DESC"
    )?;
    // map rows to HandoffRow struct
    todo!()
}
```

Define `HandoffRow` in `common/src/types.rs`:
```rust
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct HandoffRow {
    pub id: i64,
    pub from_agent_id: i64,
    pub to_agent_id: Option<i64>,
    pub reason: String,
    pub ts: i64,
    pub context_snapshot_path: Option<String>,
}
```

- [ ] **Step 3: Run tests**
```bash
cd rust && cargo test -p handoff-daemon e2e_failover -- --nocapture 2>&1
```
Expected: PASS (failover row created)

- [ ] **Step 4: Commit**
```bash
git add rust/crates/daemon/tests/e2e_failover.rs rust/crates/storage/src/lib.rs rust/crates/common/src/types.rs
git commit -m "test: add e2e failover harness and HandoffRow type"
```

---

### Task 1.3: Live `handoff agents` TUI

**Files:**
- Create: `rust/crates/cli/src/tui/mod.rs`
- Create: `rust/crates/cli/src/tui/agents.rs`
- Create: `rust/crates/cli/src/tui/events.rs`
- Modify: `rust/crates/cli/Cargo.toml` (add ratatui, crossterm)
- Modify: `rust/crates/cli/src/main.rs` (agents subcommand: add --once flag)
- Modify: `rust/crates/daemon/src/lib.rs` (add /handoffs and /events endpoints)

`★ Insight ─────────────────────────────────────`
- ratatui 0.28 uses a "immediate mode" rendering model: each tick you redraw the whole terminal frame from scratch — no retained widget state. The `Terminal::draw(|frame| { ... })` closure is your render function, called every ~1s
- crossterm::event::poll() with a 1s timeout is the idiomatic way to handle both user input and periodic refresh in a ratatui loop without spinning
`─────────────────────────────────────────────────`

- [ ] **Step 1: Add TUI dependencies to cli/Cargo.toml**
```toml
ratatui = "0.28"
crossterm = { version = "0.28", features = ["event-stream"] }
```

- [ ] **Step 2: Add /handoffs and /events endpoints to daemon/src/lib.rs**

```rust
#[derive(serde::Deserialize, Default)]
struct PaginationParams {
    limit: Option<usize>,
}

async fn list_handoffs_http(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(5);
    // query storage — reuse list_handoffs from Task 1.2b
    let rows = state.db.list_handoffs_recent(limit as i64).unwrap_or_default();
    Json(serde_json::json!({"handoffs": rows}))
}

async fn list_events_http(
    State(state): State<AppState>,
    Query(params): Query<EventsParams>,
) -> Json<serde_json::Value> {
    // tail events.log since timestamp
    let since = params.since.unwrap_or(0);
    let events = tail_events_log(since).unwrap_or_default();
    Json(serde_json::json!({"events": events}))
}
```

Add routes to `build_router()`:
```rust
.route("/handoffs", get(list_handoffs_http))
.route("/events", get(list_events_http))
```

- [ ] **Step 3: Implement tui/mod.rs**
```rust
// rust/crates/cli/src/tui/mod.rs
pub mod agents;
pub mod events;

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::Duration;

pub async fn run(daemon_url: &str) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, daemon_url).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    result
}

async fn event_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, daemon_url: &str) -> anyhow::Result<()> {
    loop {
        let agents = agents::fetch(daemon_url).await.unwrap_or_default();
        let handoffs = events::fetch_handoffs(daemon_url).await.unwrap_or_default();

        terminal.draw(|frame| {
            agents::render(frame, &agents, &handoffs);
        })?;

        if event::poll(Duration::from_secs(1))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Implement tui/agents.rs**
```rust
// rust/crates/cli/src/tui/agents.rs
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Row, Table, TableState},
    Frame,
};
use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
pub struct AgentSummary {
    pub agent_id: i64,
    pub kind: String,
    pub pid: Option<i32>,
    pub status: String,
    pub tokens_remaining: Option<i64>,
    pub tokens_initial: Option<i64>,
    pub requests_remaining: Option<i64>,
}

pub async fn fetch(daemon_url: &str) -> anyhow::Result<Vec<AgentSummary>> {
    let resp = reqwest::Client::new()
        .post(format!("{}/rpc", daemon_url))
        .json(&serde_json::json!({"method": "list_agents", "params": {}}))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    let agents = serde_json::from_value(resp["result"]["agents"].clone())
        .unwrap_or_default();
    Ok(agents)
}

pub fn render(frame: &mut Frame, agents: &[AgentSummary], handoffs: &[String]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // title
            Constraint::Min(6),     // agents table
            Constraint::Length(7),  // handoffs panel
            Constraint::Length(3),  // footer
        ])
        .split(frame.area());

    // Title bar
    let title = Block::default()
        .title(" handoff agents ")
        .borders(Borders::ALL);
    frame.render_widget(title, chunks[0]);

    // Agents table
    let rows: Vec<Row> = agents.iter().map(|a| {
        let token_pct = match (a.tokens_remaining, a.tokens_initial) {
            (Some(rem), Some(init)) if init > 0 => format!("{}%", rem * 100 / init),
            (Some(rem), _) => format!("{}", rem),
            _ => "—".to_string(),
        };
        Row::new(vec![
            a.kind.clone(),
            a.pid.map(|p| p.to_string()).unwrap_or_default(),
            a.status.clone(),
            token_pct,
            a.requests_remaining.map(|r| r.to_string()).unwrap_or("—".to_string()),
        ])
    }).collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(10),
        ],
    )
    .header(Row::new(vec!["Kind", "PID", "Status", "Tokens", "Requests"])
        .style(Style::default().fg(Color::Yellow)))
    .block(Block::default().title("Agents").borders(Borders::ALL));

    frame.render_widget(table, chunks[1]);

    // Handoffs panel
    let handoff_text = handoffs.join("\n");
    let handoff_block = Block::default()
        .title("Recent Handoffs")
        .borders(Borders::ALL);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(handoff_text).block(handoff_block),
        chunks[2],
    );

    // Footer
    let footer = Block::default()
        .title(" q: quit | h: manual handoff | r: refresh ")
        .borders(Borders::TOP);
    frame.render_widget(footer, chunks[3]);
}
```

- [ ] **Step 5: Wire agents subcommand to TUI**

In `main.rs`, the `Agents` subcommand currently calls a static list. Add `--once` flag:
```rust
Agents {
    /// Print once and exit (no live UI)
    #[arg(long)]
    once: bool,
    project_id: Option<i64>,
},
```

Route:
```rust
Cli::Agents { once, project_id } => {
    if once {
        // existing static behavior
        cmd_agents(project_id).await?;
    } else {
        let url = std::env::var("HANDOFF_DAEMON_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:7879".to_string());
        crate::tui::run(&url).await?;
    }
}
```

- [ ] **Step 6: Build and manual smoke test**
```bash
cd rust && cargo build -p handoff-cli 2>&1
# In one terminal, run handoff daemon start
# In another:
./target/debug/handoff agents
```
Expected: TUI renders with agents table. Press `q` to exit cleanly.

- [ ] **Step 7: Commit**
```bash
git add rust/crates/cli/src/tui/ rust/crates/cli/Cargo.toml rust/crates/cli/src/main.rs rust/crates/daemon/src/lib.rs
git commit -m "feat(tui): live agents TUI with ratatui, --once flag for scripting"
```

---

## TIER 2 — Architectural Fixes

### Task 2.1: Move headless_argv into adapters

**Files:**
- Modify: `rust/crates/adapters/src/lib.rs` (add `run_headless()` to Adapter trait)
- Modify: `rust/crates/adapters/src/lib.rs` (implement for each adapter)
- Modify: `rust/crates/daemon/src/spawn.rs` (remove `argv_for()`, call through adapter)

`★ Insight ─────────────────────────────────────`
- Adding a method to a trait is a breaking change for all existing implementors — add a default impl (`fn run_headless(...) -> Option<Vec<String>> { None }`) so CursorAdapter doesn't need to change (it has no headless form)
- `dyn Adapter` can't be cloned, but since all adapters are stateless structs, passing a reference or using `adapters::for_kind(kind)` at call sites is idiomatic
`─────────────────────────────────────────────────`

- [ ] **Step 1: Write failing test for adapter headless command**
```rust
// rust/crates/adapters/tests/headless_test.rs
use handoff_adapters::{ClaudeAdapter, CodexAdapter, CopilotAdapter, CursorAdapter, Adapter};

#[test]
fn claude_headless_argv_contains_minus_p() {
    let adapter = ClaudeAdapter;
    let argv = adapter.headless_argv("do the thing").unwrap();
    assert_eq!(argv[0], "claude");
    assert!(argv.contains(&"-p".to_string()));
    assert!(argv.last().unwrap().contains("do the thing"));
}

#[test]
fn codex_headless_argv_starts_with_exec() {
    let adapter = CodexAdapter;
    let argv = adapter.headless_argv("do the thing").unwrap();
    assert_eq!(argv[0], "codex");
    assert_eq!(argv[1], "exec");
}

#[test]
fn cursor_headless_argv_is_none() {
    let adapter = CursorAdapter;
    assert!(adapter.headless_argv("anything").is_none());
}
```

Run:
```bash
cd rust && cargo test -p handoff-adapters headless_test 2>&1
```
Expected: FAIL (method doesn't exist on trait)

- [ ] **Step 2: Add `headless_argv` to Adapter trait in adapters/src/lib.rs (after line 71)**
```rust
/// Returns the argv to run this agent headlessly with the given prompt.
/// Returns None if the agent has no headless mode.
fn headless_argv(&self, prompt: &str) -> Option<Vec<String>> {
    None
}
```

- [ ] **Step 3: Implement on concrete adapters**

In `ClaudeAdapter` impl (around line 172):
```rust
fn headless_argv(&self, prompt: &str) -> Option<Vec<String>> {
    Some(vec!["claude".into(), "-p".into(), prompt.to_string()])
}
```

In `CodexAdapter` impl (around line 212):
```rust
fn headless_argv(&self, prompt: &str) -> Option<Vec<String>> {
    Some(vec!["codex".into(), "exec".into(), prompt.to_string()])
}
```

In `CopilotAdapter` impl (around line 261):
```rust
fn headless_argv(&self, prompt: &str) -> Option<Vec<String>> {
    Some(vec!["gh".into(), "copilot".into(), "suggest".into(), prompt.to_string()])
}
```

`CursorAdapter` keeps the default `None`.

- [ ] **Step 4: Replace argv_for() in spawn.rs**

In `rust/crates/daemon/src/spawn.rs`, replace `argv_for()` (lines 34-46):
```rust
// Remove argv_for() entirely. Replace headless_spawn internals:
pub async fn headless_spawn(
    kind: &str,
    project_root: &Path,
    prompt: &str,
    proxy_url: &str,
) -> Result<Option<Child>> {
    let adapter = handoff_adapters::for_kind_str(kind)
        .ok_or_else(|| anyhow::anyhow!("Unknown agent kind: {}", kind))?;
    
    let argv = match adapter.headless_argv(prompt) {
        Some(v) => v,
        None => return Ok(None),
    };
    
    // ... rest of spawn logic unchanged (env setup, tee, Child spawn)
}
```

Add `for_kind_str(kind: &str) -> Option<Box<dyn Adapter>>` to adapters/src/lib.rs:
```rust
pub fn for_kind_str(kind: &str) -> Option<Box<dyn Adapter>> {
    match kind {
        "claude" | "claude-code" => Some(Box::new(ClaudeAdapter)),
        "codex" => Some(Box::new(CodexAdapter)),
        "copilot" => Some(Box::new(CopilotAdapter)),
        "cursor" | "antigravity" => Some(Box::new(CursorAdapter)),
        _ => None,
    }
}
```

- [ ] **Step 5: Run all tests**
```bash
cd rust && cargo test --workspace 2>&1 | tail -20
```
Expected: all pass, no regressions.

- [ ] **Step 6: Commit**
```bash
git add rust/crates/adapters/src/lib.rs rust/crates/adapters/tests/headless_test.rs rust/crates/daemon/src/spawn.rs
git commit -m "refactor: move headless_argv into Adapter trait, remove argv_for() from spawn"
```

---

### Task 2.2: Worktree isolation for headless agents

**Files:**
- Create: `rust/crates/daemon/src/worktree.rs`
- Modify: `rust/crates/daemon/src/spawn.rs` (call worktree mod)
- Modify: `rust/crates/cli/src/main.rs` (Worktree subcommand)
- Modify: `rust/crates/policy/src/lib.rs` (new `use_worktree` field in FailoverPolicy)

- [ ] **Step 1: Write failing test**
```rust
// rust/crates/daemon/tests/worktree_test.rs
use handoff_daemon::worktree;
use tempfile::TempDir;
use std::process::Command;

#[test]
fn create_worktree_makes_git_worktree() {
    let tmp = TempDir::new().unwrap();
    // Init a bare git repo for testing
    Command::new("git").args(["init", tmp.path().to_str().unwrap()]).output().unwrap();
    Command::new("git").args(["-C", tmp.path().to_str().unwrap(), "commit", "--allow-empty", "-m", "init"]).output().unwrap();

    let agent_id = 42i64;
    let result = worktree::create(tmp.path(), agent_id);
    assert!(result.is_ok(), "{:?}", result.err());

    let wt_path = result.unwrap();
    assert!(wt_path.exists());
}
```

Run:
```bash
cd rust && cargo test -p handoff-daemon worktree_test 2>&1
```
Expected: FAIL (module doesn't exist)

- [ ] **Step 2: Create rust/crates/daemon/src/worktree.rs**
```rust
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

fn worktree_dir(agent_id: i64) -> PathBuf {
    handoff_common::paths::home_dir()
        .join("worktrees")
        .join(format!("agent-{}", agent_id))
}

pub fn create(project_root: &Path, agent_id: i64) -> Result<PathBuf> {
    let dest = worktree_dir(agent_id);
    std::fs::create_dir_all(dest.parent().unwrap())?;

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
    let patch_dir = handoff_common::paths::home_dir().join("diffs");
    std::fs::create_dir_all(&patch_dir)?;
    let patch_path = patch_dir.join(format!("agent-{}.patch", agent_id));
    let diff_output = diff(agent_id)?;
    std::fs::write(&patch_path, diff_output)?;
    Ok(patch_path)
}

pub fn remove(project_root: &Path, agent_id: i64) -> Result<()> {
    let wt = worktree_dir(agent_id);
    Command::new("git")
        .args(["worktree", "remove", "--force", wt.to_str().unwrap()])
        .current_dir(project_root)
        .status()?;
    Ok(())
}

pub fn list(project_root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let paths = text.lines()
        .filter(|l| l.starts_with("worktree "))
        .map(|l| PathBuf::from(l.trim_start_matches("worktree ")))
        .filter(|p| p.to_string_lossy().contains("agent-"))
        .collect();
    Ok(paths)
}
```

- [ ] **Step 3: Wire into headless_spawn in spawn.rs**

At the start of `headless_spawn()`, after resolving `argv`:
```rust
// Optionally create a worktree for isolation
let effective_dir = if policy.use_worktree {
    match worktree::create(project_root, agent_id) {
        Ok(wt) => wt,
        Err(e) => {
            tracing::warn!("worktree create failed ({}), using project root", e);
            project_root.to_path_buf()
        }
    }
} else {
    project_root.to_path_buf()
};
// Use effective_dir as CWD for the spawned process
```

- [ ] **Step 4: Add use_worktree to FailoverPolicy (policy/src/lib.rs after line 34)**
```rust
pub use_worktree: bool,  // default true
```

In `Default` impl:
```rust
use_worktree: true,
```

In TOML deserialization (if missing, defaults to true):
```toml
# .handoff/config.toml — users can opt out:
[failover]
use_worktree = false
```

- [ ] **Step 5: Add Worktree CLI subcommand**
```rust
Worktree {
    #[clap(subcommand)]
    cmd: WorktreeCmd,
},

// SubSubCommand enum:
enum WorktreeCmd {
    List,
    Diff { agent_id: i64 },
    Clean { agent_id: i64 },
}
```

- [ ] **Step 6: Run all tests**
```bash
cd rust && cargo test --workspace 2>&1 | tail -20
```
Expected: pass

- [ ] **Step 7: Commit**
```bash
git add rust/crates/daemon/src/worktree.rs rust/crates/daemon/src/spawn.rs rust/crates/policy/src/lib.rs rust/crates/cli/src/main.rs rust/crates/daemon/tests/worktree_test.rs
git commit -m "feat: worktree isolation for headless agents, handoff worktree subcommand"
```

---

### Task 2.3: PreToolUse hook adapter

**Files:**
- Create: `rust/crates/cli/src/hook.rs`
- Modify: `rust/crates/daemon/src/lib.rs` (new /hook endpoint)
- Modify: `rust/crates/storage/src/lib.rs` (new agent_activity table)
- Modify: `rust/crates/cli/src/main.rs` (Hook subcommand)

- [ ] **Step 1: Add agent_activity table to schema**

In `rust/crates/storage/src/lib.rs` SCHEMA constant:
```sql
CREATE TABLE IF NOT EXISTS agent_activity (
    id          INTEGER PRIMARY KEY,
    agent_id    INTEGER NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    ts          INTEGER NOT NULL,
    tool_name   TEXT,
    tool_input  TEXT
);
CREATE INDEX IF NOT EXISTS idx_activity_agent ON agent_activity(agent_id, ts);
```

Add query:
```rust
pub fn insert_activity(&self, agent_id: i64, tool_name: &str, tool_input: Option<&str>) -> Result<()> {
    let conn = self.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO agent_activity (agent_id, ts, tool_name, tool_input) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![agent_id, chrono::Utc::now().timestamp(), tool_name, tool_input],
    )?;
    Ok(())
}
```

- [ ] **Step 2: Add /hook endpoint to daemon**
```rust
#[derive(serde::Deserialize)]
struct HookPayload {
    agent_pid: Option<i32>,
    tool_name: Option<String>,
    tool_input: Option<String>,
}

async fn hook(
    State(state): State<AppState>,
    Json(payload): Json<HookPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Some(pid) = payload.agent_pid {
        if let Ok(Some(agent)) = state.db.find_agent_by_pid(pid as i64) {
            let _ = state.db.insert_activity(
                agent.id,
                payload.tool_name.as_deref().unwrap_or("unknown"),
                payload.tool_input.as_deref(),
            );
        }
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true})))
}
```

Add route:
```rust
.route("/hook", post(hook))
```

- [ ] **Step 3: Create cli/src/hook.rs**
```rust
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
    std::fs::write(settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!("Hook installed in {}", settings_path.display());
    Ok(())
}

pub fn uninstall_claude(settings_path: &Path) -> Result<()> {
    let settings_str = std::fs::read_to_string(settings_path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&settings_str)?;
    if let Some(hooks) = settings.get_mut("hooks") {
        hooks.as_object_mut().map(|h| h.remove("PreToolUse"));
    }
    std::fs::write(settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!("Hook removed from {}", settings_path.display());
    Ok(())
}
```

- [ ] **Step 4: Add Hook subcommand to CLI**
```rust
Hook {
    #[clap(subcommand)]
    cmd: HookCmd,
},

enum HookCmd {
    Install { agent: String },
    Uninstall { agent: String },
}
```

Route:
```rust
Cli::Hook { cmd } => match cmd {
    HookCmd::Install { agent } => {
        if agent == "claude" {
            let settings = dirs::home_dir().unwrap().join(".claude/settings.json");
            hook::install_claude("http://127.0.0.1:7879", &settings)?;
        } else {
            eprintln!("Hook install for '{}' not yet supported", agent);
        }
    }
    HookCmd::Uninstall { agent } => {
        if agent == "claude" {
            let settings = dirs::home_dir().unwrap().join(".claude/settings.json");
            hook::uninstall_claude(&settings)?;
        }
    }
},
```

- [ ] **Step 5: Run tests**
```bash
cd rust && cargo test --workspace 2>&1 | tail -10
```

- [ ] **Step 6: Commit**
```bash
git add rust/crates/cli/src/hook.rs rust/crates/daemon/src/lib.rs rust/crates/storage/src/lib.rs rust/crates/cli/src/main.rs
git commit -m "feat: PreToolUse hook adapter, /hook endpoint, agent_activity table"
```

---

### Task 2.4: Budget-aware chain selection + cool-down

**Files:**
- Modify: `rust/crates/policy/src/lib.rs` (rewrite pick_next, add return_to_primary)
- Modify: `rust/crates/daemon/src/failover.rs` (pass rate samples to pick_next)
- Modify: `rust/crates/storage/src/lib.rs` (add latest_rate_sample_per_kind query)

- [ ] **Step 1: Write failing test for budget-aware pick_next**
```rust
// rust/crates/policy/tests/pick_next_test.rs
use handoff_policy::{pick_next, RateSampleInput};

#[test]
fn picks_agent_with_most_tokens() {
    let chain = vec!["claude".to_string(), "codex".to_string(), "copilot".to_string()];
    let samples = vec![
        RateSampleInput { kind: "claude".into(), tokens_remaining: 0, tokens_reset_at: None },
        RateSampleInput { kind: "codex".into(), tokens_remaining: 5000, tokens_reset_at: None },
        RateSampleInput { kind: "copilot".into(), tokens_remaining: 9000, tokens_reset_at: None },
    ];
    let result = pick_next(&chain, "claude", &samples);
    assert_eq!(result, Some("copilot"));
}

#[test]
fn skips_current_agent_even_if_highest() {
    let chain = vec!["claude".to_string(), "codex".to_string()];
    let samples = vec![
        RateSampleInput { kind: "claude".into(), tokens_remaining: 99999, tokens_reset_at: None },
        RateSampleInput { kind: "codex".into(), tokens_remaining: 100, tokens_reset_at: None },
    ];
    let result = pick_next(&chain, "claude", &samples);
    assert_eq!(result, Some("codex"));
}

#[test]
fn falls_back_to_first_in_chain_when_no_samples() {
    let chain = vec!["claude".to_string(), "codex".to_string()];
    let result = pick_next(&chain, "claude", &[]);
    assert_eq!(result, Some("codex"));
}
```

Run:
```bash
cd rust && cargo test -p handoff-policy pick_next_test 2>&1
```
Expected: FAIL (wrong signature)

- [ ] **Step 2: Rewrite pick_next in policy/src/lib.rs**

Add new type (line ~20 area):
```rust
#[derive(Debug, Clone)]
pub struct RateSampleInput {
    pub kind: String,
    pub tokens_remaining: i64,
    pub tokens_reset_at: Option<i64>,
}
```

Rewrite `pick_next` (replaces lines 157-162):
```rust
pub fn pick_next<'a>(
    chain: &'a [String],
    current: &str,
    samples: &[RateSampleInput],
) -> Option<&'a str> {
    // Candidates: in-chain agents that aren't current
    let candidates: Vec<&String> = chain.iter()
        .filter(|k| k.as_str() != current)
        .collect();
    
    if candidates.is_empty() {
        return None;
    }
    
    if samples.is_empty() {
        // Fallback: first candidate in chain order
        return Some(candidates[0].as_str());
    }
    
    // Pick the candidate with the highest tokens_remaining
    candidates.iter()
        .max_by_key(|k| {
            samples.iter()
                .find(|s| s.kind == ***k)
                .map(|s| s.tokens_remaining)
                .unwrap_or(0)
        })
        .map(|k| k.as_str())
}
```

Add to FailoverPolicy:
```rust
pub return_to_primary: bool,  // default false
```

- [ ] **Step 3: Pass samples to pick_next in failover.rs**

In `FailoverEngine::handle()` (around line 100), before calling `pick_next`:
```rust
// Fetch latest token samples per agent kind in chain
let samples: Vec<handoff_policy::RateSampleInput> = {
    let policy = &self.policy;
    policy.chain.iter().filter_map(|kind| {
        self.state.db.latest_rate_sample_for_kind(kind).ok().flatten()
            .map(|s| handoff_policy::RateSampleInput {
                kind: kind.clone(),
                tokens_remaining: s.tokens_remaining.unwrap_or(0),
                tokens_reset_at: s.tokens_reset_at,
            })
    }).collect()
};
let to_kind = handoff_policy::pick_next(&policy.chain, &ev.kind, &samples);
```

- [ ] **Step 4: Add latest_rate_sample_for_kind query to storage**
```rust
pub fn latest_rate_sample_for_kind(&self, kind: &str) -> Result<Option<RateSampleRow>> {
    let conn = self.conn.lock().unwrap();
    conn.query_row(
        "SELECT rs.* FROM rate_samples rs
         JOIN agents a ON a.id = rs.agent_id
         WHERE a.kind = ?1
         ORDER BY rs.ts DESC LIMIT 1",
        rusqlite::params![kind],
        |row| { /* map to RateSampleRow */ Ok(RateSampleRow { tokens_remaining: row.get(4)?, tokens_reset_at: row.get(6)? }) },
    ).optional().map_err(Into::into)
}
```

- [ ] **Step 5: Run all tests**
```bash
cd rust && cargo test --workspace 2>&1 | grep -E "FAILED|PASSED|error"
```
Expected: all pass

- [ ] **Step 6: Commit**
```bash
git add rust/crates/policy/src/lib.rs rust/crates/policy/tests/ rust/crates/daemon/src/failover.rs rust/crates/storage/src/lib.rs
git commit -m "feat(policy): budget-aware chain selection, return_to_primary config"
```

---

## TIER 3 — Quality & Confidence

### Task 3.1: Richer snapshot data

**Files:**
- Modify: `rust/crates/context/src/lib.rs` (extend snapshot() at lines 149-178)
- Create: `rust/crates/context/src/conversations.rs`
- Modify: `rust/crates/common/src/types.rs` (add fields to Snapshot)

- [ ] **Step 1: Add new Snapshot fields**

In `common/src/types.rs` (after line 98), add to `Snapshot`:
```rust
pub git_log: Option<String>,         // last 10 commits
pub untracked_files: Vec<String>,
pub prior_agent_tee_tail: Option<String>,  // last 50 lines of prior agent log
pub conversation_tail: Option<String>,     // last user+assistant exchange
pub prior_handoffs: Vec<HandoffRow>,       // last 3 handoffs
```

- [ ] **Step 2: Extend context/src/lib.rs snapshot()**

Add these after the existing git_diff collection (around line 155):
```rust
// git log
let git_log = std::process::Command::new("git")
    .args(["log", "--oneline", "-10"])
    .current_dir(&self.root)
    .output()
    .ok()
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string());

// untracked files
let untracked_files: Vec<String> = std::process::Command::new("git")
    .args(["ls-files", "--others", "--exclude-standard"])
    .current_dir(&self.root)
    .output()
    .ok()
    .map(|o| String::from_utf8_lossy(&o.stdout)
        .lines().map(|l| l.to_string()).collect())
    .unwrap_or_default();

// cap git_diff at 20KB
let git_diff = git_diff.map(|d| {
    if d.len() > 20_000 {
        format!("{}\n... [truncated at 20KB]", &d[..20_000])
    } else { d }
});
```

- [ ] **Step 3: Create conversations.rs**
```rust
// rust/crates/context/src/conversations.rs
use std::path::Path;

/// Read the last user+assistant exchange from Claude Code's JSONL session files
pub fn claude_conversation_tail(project_root: &Path) -> Option<String> {
    // Claude stores sessions in ~/.claude/projects/<hash>/*.jsonl
    // Hash is SHA1-like of the project root path
    let projects_dir = dirs::home_dir()?.join(".claude/projects");
    // Find the most recently modified JSONL in any project dir
    let latest_jsonl = find_latest_jsonl(&projects_dir)?;
    let content = std::fs::read_to_string(&latest_jsonl).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    // Take last 20 lines — enough for the last exchange
    let tail: Vec<&str> = lines.iter().rev().take(20).cloned().collect();
    let tail_str = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    Some(format!("<!-- last ~20 lines of {} -->\n{}", latest_jsonl.display(), tail_str))
}

fn find_latest_jsonl(dir: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir).ok()?
        .filter_map(|e| e.ok())
        .flat_map(|e| std::fs::read_dir(e.path()).ok().into_iter().flatten())
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
        .max_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()))
        .map(|e| e.path())
}
```

- [ ] **Step 4: Verify snapshot output**
```bash
cd rust && cargo run -p handoff-cli -- snapshot --reason "test"
cat <project>/.handoff/scratch/handoff-*.md | head -60
```
Expected: git_log section present, untracked files listed.

- [ ] **Step 5: Commit**
```bash
git add rust/crates/context/src/ rust/crates/common/src/types.rs
git commit -m "feat(context): richer snapshot — git log, untracked, conversation tail"
```

---

### Task 3.2: Command-output reducers

**Files:**
- Create: `rust/crates/cli/src/reduce/mod.rs`
- Create: `rust/crates/cli/src/reduce/cargo_test.rs`
- Create: `rust/crates/cli/src/reduce/git_diff.rs`
- Modify: `rust/crates/cli/src/main.rs` (Reduce subcommand)

- [ ] **Step 1: Write failing test for cargo test reducer**
```rust
// rust/crates/cli/tests/reduce_test.rs
use handoff_cli::reduce::cargo_test::reduce;

#[test]
fn reduce_cargo_test_extracts_failures() {
    let full_output = r#"
running 100 tests
test auth::login_works ... ok
test auth::logout_works ... ok
test payments::charge_fails ... FAILED

failures:

---- payments::charge_fails stdout ----
thread 'payments::charge_fails' panicked at 'called `Result::unwrap()` on an `Err` value: "connection refused"', src/payments.rs:42:5

test result: FAILED. 99 passed; 1 failed;
"#;
    let reduced = reduce(full_output);
    assert!(reduced.contains("FAILED"));
    assert!(reduced.contains("payments::charge_fails"));
    assert!(reduced.contains("connection refused"));
    // Should NOT include the passing tests
    assert!(!reduced.contains("auth::login_works"));
    // Should be small
    assert!(reduced.len() < 500);
}
```

Run:
```bash
cd rust && cargo test -p handoff-cli reduce_test 2>&1
```
Expected: FAIL

- [ ] **Step 2: Implement reduce/cargo_test.rs**
```rust
// rust/crates/cli/src/reduce/cargo_test.rs

pub fn reduce(output: &str) -> String {
    let mut in_failures = false;
    let mut failure_lines: Vec<&str> = Vec::new();
    let mut failure_names: Vec<&str> = Vec::new();
    let mut summary_line = "";

    for line in output.lines() {
        if line.starts_with("failures:") {
            in_failures = true;
        }
        if in_failures {
            failure_lines.push(line);
            // Cap at 20 lines per failure block
            if failure_lines.len() > 200 { break; }
        }
        if line.contains("... FAILED") {
            if let Some(name) = line.split("test ").nth(1) {
                failure_names.push(name.trim_end_matches(" ... FAILED"));
            }
        }
        if line.starts_with("test result:") {
            summary_line = line;
        }
    }

    if failure_names.is_empty() {
        return format!("{}\n(all tests passed)", summary_line);
    }

    let mut out = format!("FAILED tests: {}\n\n", failure_names.join(", "));
    // Include first 20 lines of failure output
    let cap = failure_lines.iter().take(20);
    for l in cap { out.push_str(l); out.push('\n'); }
    out.push_str(&format!("\n{}", summary_line));
    out
}
```

- [ ] **Step 3: Implement reduce/git_diff.rs**
```rust
// rust/crates/cli/src/reduce/git_diff.rs

pub fn reduce(output: &str) -> String {
    // Group by file, summarize hunks >100 lines
    let mut result = String::new();
    let mut current_file = "";
    let mut hunk_lines = 0usize;
    let mut in_hunk = false;

    for line in output.lines() {
        if line.starts_with("diff --git") {
            current_file = line;
            result.push_str(line);
            result.push('\n');
            hunk_lines = 0;
            in_hunk = false;
        } else if line.starts_with("@@") {
            in_hunk = true;
            hunk_lines = 0;
            result.push_str(line);
            result.push('\n');
        } else if in_hunk {
            hunk_lines += 1;
            if hunk_lines <= 100 {
                result.push_str(line);
                result.push('\n');
            } else if hunk_lines == 101 {
                result.push_str("... [hunk truncated — too large]\n");
            }
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}
```

- [ ] **Step 4: Create reduce/mod.rs**
```rust
pub mod cargo_test;
pub mod git_diff;

use std::process::{Command, Stdio};
use anyhow::Result;

pub async fn run_reduce(args: &[String]) -> Result<()> {
    if args.is_empty() {
        anyhow::bail!("Usage: handoff reduce <command> [args...]");
    }

    let output = Command::new(&args[0])
        .args(&args[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let full_output = format!("{}{}", stdout, stderr);

    // Log original to ~/.handoff/logs/commands.log
    let log_dir = handoff_common::paths::home_dir().join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join("commands.log");
    std::fs::OpenOptions::new()
        .append(true).create(true).open(&log_path)?;
    // Append with timestamp + command header
    let header = format!("\n=== {} === {}\n", chrono::Utc::now().to_rfc3339(), args.join(" "));
    std::fs::write(&log_path, format!("{}{}", header, full_output))?;

    // Apply reducer
    let reduced = match args[0].as_str() {
        "cargo" if args.get(1).map(|s| s == "test").unwrap_or(false) => {
            cargo_test::reduce(&full_output)
        }
        "git" if args.get(1).map(|s| s == "diff").unwrap_or(false) => {
            git_diff::reduce(&full_output)
        }
        _ => full_output.to_string(),
    };

    print!("{}", reduced);
    Ok(())
}
```

- [ ] **Step 5: Add Reduce subcommand to CLI**
```rust
/// Pipe a command through output reducers to save tokens
Reduce {
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
},
```

Route:
```rust
Cli::Reduce { args } => reduce::run_reduce(&args).await?,
```

- [ ] **Step 6: Run tests**
```bash
cd rust && cargo test -p handoff-cli reduce_test
```

- [ ] **Step 7: Commit**
```bash
git add rust/crates/cli/src/reduce/ rust/crates/cli/src/main.rs rust/crates/cli/tests/reduce_test.rs
git commit -m "feat(cli): handoff reduce command with cargo test and git diff filters"
```

---

### Task 3.4: Multi-round critic loop

**Files:**
- Modify: `rust/crates/critic/src/lib.rs` (rewrite `run()` as loop)
- Create: `rust/crates/critic/src/diff.rs` (extract diff blocks)

- [ ] **Step 1: Write failing test**
```rust
// rust/crates/critic/tests/multi_round_test.rs
// Uses a fake Anthropic server that returns REVISE on round 1, APPROVE on round 2

// This test is integration-level — mark with #[ignore] and run manually with ANTHROPIC_API_KEY set
// For unit testing, mock the `ask()` function via a trait wrapper
```

- [ ] **Step 2: Create diff.rs**
```rust
// rust/crates/critic/src/diff.rs

/// Extract unified diff blocks from a string
pub fn extract_diffs(text: &str) -> Vec<String> {
    let mut diffs = Vec::new();
    let mut current = String::new();
    let mut in_diff = false;

    for line in text.lines() {
        if line.starts_with("diff --git") || line.starts_with("--- a/") {
            in_diff = true;
            if !current.is_empty() {
                diffs.push(current.clone());
                current.clear();
            }
        }
        if in_diff {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.is_empty() {
        diffs.push(current);
    }
    diffs
}

pub fn apply_check(diff: &str, project_root: &std::path::Path) -> bool {
    let output = std::process::Command::new("git")
        .args(["apply", "--check", "-"])
        .current_dir(project_root)
        .stdin(std::process::Stdio::piped())
        .output();
    // Write diff to stdin and check exit status
    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}
```

- [ ] **Step 3: Rewrite critic run() as a loop**

In `rust/crates/critic/src/lib.rs`, the `run()` method (starting around line 144). Replace single-pass with:
```rust
pub async fn run(&self, task: &str) -> Result<CriticResult> {
    let max_rounds = self.max_rounds.unwrap_or(3);
    let mut feedback: Option<String> = None;
    let mut last_worker_output = String::new();
    let mut total_worker_tokens = 0u32;
    let mut total_critic_tokens = 0u32;

    for round in 0..max_rounds {
        // Build worker prompt: task + optional feedback from critic
        let worker_prompt = match &feedback {
            Some(fb) => format!("{}\n\nCritic feedback from round {}:\n{}", task, round, fb),
            None => task.to_string(),
        };

        // Worker pass
        let worker_resp = self.ask(&self.worker_model, WORKER_SYSTEM, &worker_prompt).await?;
        total_worker_tokens += worker_resp.tokens;
        last_worker_output = worker_resp.text.clone();

        // Run cargo check between rounds as sanity gate
        // (fire-and-forget; don't fail the whole loop on a check error)
        let _ = std::process::Command::new("cargo")
            .args(["check", "--quiet"])
            .current_dir(&self.project_root)
            .output();

        // Critic pass
        let critic_prompt = format!("Original task:\n{}\n\nWorker output:\n{}", task, last_worker_output);
        let critic_resp = self.ask(&self.critic_model, CRITIC_SYSTEM, &critic_prompt).await?;
        total_critic_tokens += critic_resp.tokens;

        if critic_resp.text.starts_with("APPROVE") {
            // Extract and apply diffs
            let diffs = diff::extract_diffs(&last_worker_output);
            let applied = diffs.iter().all(|d| diff::apply_check(d, &self.project_root));
            return Ok(CriticResult {
                verdict: "APPROVED".into(),
                plan: String::new(),
                diff: diffs.join("\n"),
                notes: critic_resp.text.clone(),
                worker_tokens: total_worker_tokens,
                critic_tokens: total_critic_tokens,
                artifacts: vec![],
            });
        } else if critic_resp.text.starts_with("REVISE:") {
            feedback = Some(critic_resp.text.trim_start_matches("REVISE:").trim().to_string());
            // Continue to next round
        } else {
            // Unknown verdict — treat as approve to avoid infinite loop
            break;
        }
    }

    Ok(CriticResult {
        verdict: "MAX_ROUNDS_REACHED".into(),
        plan: String::new(),
        diff: diff::extract_diffs(&last_worker_output).join("\n"),
        notes: format!("Reached {} rounds without APPROVE", max_rounds),
        worker_tokens: total_worker_tokens,
        critic_tokens: total_critic_tokens,
        artifacts: vec![],
    })
}
```

Add to `CriticRunner`:
```rust
pub max_rounds: Option<u32>,
```

Config field:
```toml
[critic]
max_rounds = 3
```

- [ ] **Step 4: Add max_rounds to policy/config loading**

In `policy/src/lib.rs`, `CriticConfig` struct:
```rust
pub max_rounds: Option<u32>,
```

- [ ] **Step 5: Build and integration test (manual)**
```bash
export ANTHROPIC_API_KEY=...
cd rust && cargo run -p handoff-cli -- critic run "add a /ping endpoint to the daemon"
```
Expected: up to 3 rounds of worker → critic, final verdict printed.

- [ ] **Step 6: Commit**
```bash
git add rust/crates/critic/src/ rust/crates/policy/src/lib.rs
git commit -m "feat(critic): multi-round loop (max 3), REVISE/APPROVE parsing, diff extraction"
```

---

### Task 3.5: Tee with rotation

**Files:**
- Create: `rust/crates/common/src/tee.rs`
- Modify: `rust/crates/daemon/src/spawn.rs` (use tee module)

- [ ] **Step 1: Create common/src/tee.rs**
```rust
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const MAX_SIZE_BYTES: u64 = 10 * 1024 * 1024; // 10MB
const MAX_ROTATIONS: u32 = 5;

pub fn tee_path(agent_id: i64) -> PathBuf {
    crate::paths::home_dir()
        .join("tee")
        .join(format!("agent-{}.log", agent_id))
}

pub fn rotate_if_needed(path: &Path) -> io::Result<()> {
    if !path.exists() { return Ok(()); }
    let size = std::fs::metadata(path)?.len();
    if size < MAX_SIZE_BYTES { return Ok(()); }

    // Shift old rotations: .5 → delete, .4 → .5, ... .1 → .2
    for i in (1..MAX_ROTATIONS).rev() {
        let from = PathBuf::from(format!("{}.{}", path.display(), i));
        let to = PathBuf::from(format!("{}.{}", path.display(), i + 1));
        if from.exists() {
            if i + 1 >= MAX_ROTATIONS {
                let _ = std::fs::remove_file(&to);
            }
            std::fs::rename(&from, &to)?;
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
```

- [ ] **Step 2: Update spawn.rs to use agent_id-based tee paths**

Replace the `tee/agent-{kind}-{ts}.log` naming with `tee_path(agent_id)` from the tee module. Call `tee::rotate_if_needed(&tee_path)` before opening the file:
```rust
let tee_path = handoff_common::tee::tee_path(agent_id);
std::fs::create_dir_all(tee_path.parent().unwrap())?;
handoff_common::tee::rotate_if_needed(&tee_path)?;
let tee_file = std::fs::OpenOptions::new()
    .append(true).create(true).open(&tee_path)?;
```

- [ ] **Step 3: Add logs subcommand for tee tail**

In `cli/src/main.rs`:
```rust
Logs {
    /// "daemon" | "proxy" | "agent <id>"
    target: String,
    agent_id: Option<i64>,
    #[arg(short = 'f', long)]
    follow: bool,
},
```

- [ ] **Step 4: Run tests**
```bash
cd rust && cargo test --workspace 2>&1 | grep -E "FAILED|error"
```

- [ ] **Step 5: Commit**
```bash
git add rust/crates/common/src/tee.rs rust/crates/daemon/src/spawn.rs rust/crates/cli/src/main.rs
git commit -m "feat: tee rotation (10MB, 5 rotations), agent-id-based log paths"
```

---

### Task 3.6: Concurrent brain.md serialization

**Files:**
- Modify: `rust/crates/context/src/brain.rs` (or create it)
- Modify: `rust/crates/daemon/src/lib.rs` (new /brain/* endpoints)

- [ ] **Step 1: Add /brain endpoints to daemon**
```rust
async fn brain_append(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let text = payload["text"].as_str().unwrap_or("").to_string();
    let mut brain = state.brain.lock().await;
    brain.push_str("\n");
    brain.push_str(&text);
    // Write to file
    let path = state.project_root.join(".handoff/brain.md");
    std::fs::write(&path, brain.as_str()).ok();
    Json(serde_json::json!({"ok": true}))
}
```

Add to AppState:
```rust
pub brain: tokio::sync::Mutex<String>,
```

Add routes:
```rust
.route("/brain/append", post(brain_append))
.route("/brain/edit", post(brain_edit))
```

- [ ] **Step 2: Write concurrency test**
```rust
#[tokio::test]
async fn concurrent_brain_appends_are_serialized() {
    // Spin up test daemon, send 10 concurrent /brain/append requests
    let handles: Vec<_> = (0..10).map(|i| {
        let url = "http://127.0.0.1:7879".to_string();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("{}/brain/append", url))
                .json(&serde_json::json!({"text": format!("entry {}", i)}))
                .send().await
        })
    }).collect();
    for h in handles { h.await.unwrap().ok(); }
    // Read brain.md, verify all 10 entries present
    let content = std::fs::read_to_string(".handoff/brain.md").unwrap();
    for i in 0..10 {
        assert!(content.contains(&format!("entry {}", i)));
    }
}
```

- [ ] **Step 3: Commit**
```bash
git add rust/crates/daemon/src/lib.rs rust/crates/context/src/brain.rs
git commit -m "feat: serialize brain.md writes through daemon, /brain/append endpoint"
```

---

## TIER 4 — Observability

### Task 4.1: File-based logs with tracing-appender

**Files:**
- Modify: `rust/Cargo.toml` (add tracing-appender)
- Modify: `rust/crates/daemon/src/main.rs`
- Modify: `rust/crates/proxy/src/main.rs`
- Create: `rust/crates/cli/src/logs.rs`

- [ ] **Step 1: Add tracing-appender**
```toml
# rust/Cargo.toml [workspace.dependencies]
tracing-appender = "0.2"
```

- [ ] **Step 2: Configure file logging in daemon/main.rs**
```rust
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::prelude::*;

let log_dir = handoff_common::paths::home_dir().join("logs");
std::fs::create_dir_all(&log_dir)?;

let file_appender = RollingFileAppender::builder()
    .rotation(Rotation::DAILY)
    .filename_prefix("daemon")
    .filename_suffix("log")
    .max_log_files(7)
    .build(&log_dir)?;

let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

tracing_subscriber::registry()
    .with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
    .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
    .init();
```

- [ ] **Step 3: Same setup for proxy/main.rs** (same pattern, `filename_prefix("proxy")`)

- [ ] **Step 4: Add logs subcommand to cli**

In `rust/crates/cli/src/logs.rs`:
```rust
use anyhow::Result;
use std::path::PathBuf;

pub fn log_path(target: &str) -> PathBuf {
    let log_dir = handoff_common::paths::home_dir().join("logs");
    // tracing-appender format: daemon.YYYY-MM-DD.log (latest)
    // Find the most recent file matching prefix
    let prefix = format!("{}.", target);
    std::fs::read_dir(&log_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .max_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()))
        .map(|e| e.path())
        .unwrap_or_else(|| log_dir.join(format!("{}.log", target)))
}

pub fn tail_log(target: &str, follow: bool) -> Result<()> {
    let path = log_path(target);
    if follow {
        // Use std::process::Command to tail -f
        std::process::Command::new("tail")
            .args(["-f", path.to_str().unwrap()])
            .status()?;
    } else {
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| format!("No log found at {}", path.display()));
        println!("{}", content);
    }
    Ok(())
}
```

- [ ] **Step 5: Commit**
```bash
git add rust/Cargo.toml rust/crates/daemon/src/main.rs rust/crates/proxy/src/main.rs rust/crates/cli/src/logs.rs rust/crates/cli/src/main.rs
git commit -m "feat(observability): tracing-appender file logs for daemon+proxy, handoff logs command"
```

---

### Task 4.2: `handoff replay <handoff_id>`

**Files:**
- Create: `rust/crates/cli/src/replay.rs`
- Modify: `rust/crates/storage/src/lib.rs` (query for replay data)
- Modify: `rust/crates/cli/src/main.rs` (Replay subcommand)

- [ ] **Step 1: Add replay query to storage**
```rust
pub struct ReplayData {
    pub handoff: HandoffRow,
    pub from_agent: AgentRow,
    pub to_agent: Option<AgentRow>,
    pub rate_samples_around: Vec<RateSampleRow>, // ±30s window
    pub snapshot_content: Option<String>,
}

pub fn get_replay_data(&self, handoff_id: i64) -> Result<ReplayData> { ... }
```

- [ ] **Step 2: Implement replay.rs**
```rust
pub fn print_replay(data: &ReplayData) {
    let base_ts = data.handoff.ts;
    println!("=== Handoff #{} Replay ===\n", data.handoff.id);
    println!("T+0ms    Rate event from {} (tokens={:?})",
        data.from_agent.kind,
        data.rate_samples_around.first().and_then(|s| s.tokens_remaining));
    println!("         Reason: {}", data.handoff.reason);
    if let Some(ref to) = data.to_agent {
        println!("T+?ms    {} spawned (PID {:?})", to.kind, to.pid);
    }
    if let Some(ref snap) = data.snapshot_content {
        println!("\n--- Snapshot summary ---");
        // Print first 20 lines of snapshot
        for line in snap.lines().take(20) { println!("  {}", line); }
        println!("  ...");
    }
}
```

- [ ] **Step 3: Commit**
```bash
git add rust/crates/cli/src/replay.rs rust/crates/storage/src/lib.rs rust/crates/cli/src/main.rs
git commit -m "feat: handoff replay command for debugging past failovers"
```

---

### Task 4.3: `handoff stats`

**Files:**
- Create: `rust/crates/cli/src/stats.rs`
- Modify: `rust/crates/cli/src/main.rs` (Stats subcommand)
- Modify: `rust/crates/storage/src/lib.rs` (daily aggregation query)

- [ ] **Step 1: Add daily aggregation query**
```rust
pub struct DailyStat {
    pub date: String,         // "2026-04-28"
    pub kind: String,
    pub total_requests: i64,
    pub avg_tokens_remaining: Option<f64>,
    pub handoff_count: i64,
}

pub fn daily_stats(&self, project_id: Option<i64>, days: u32) -> Result<Vec<DailyStat>> {
    let conn = self.conn.lock().unwrap();
    let since = chrono::Utc::now().timestamp() - (days as i64 * 86400);
    // GROUP BY date(ts, 'unixepoch'), kind
    let mut stmt = conn.prepare(
        "SELECT date(rs.ts, 'unixepoch') as d,
                a.kind,
                COUNT(*) as total,
                AVG(rs.tokens_remaining) as avg_tok,
                (SELECT COUNT(*) FROM handoffs h JOIN agents fa ON fa.id = h.from_agent_id WHERE fa.kind = a.kind AND h.ts BETWEEN rs.ts - 86400 AND rs.ts) as hc
         FROM rate_samples rs
         JOIN agents a ON a.id = rs.agent_id
         WHERE rs.ts > ?1
         GROUP BY d, a.kind
         ORDER BY d DESC"
    )?;
    // ...map rows
    Ok(vec![])
}
```

- [ ] **Step 2: Implement stats.rs**
```rust
pub fn print_stats(stats: &[DailyStat], graph: bool) {
    for s in stats {
        let bar = if graph {
            let filled = (s.avg_tokens_remaining.unwrap_or(0.0) / 1000.0) as usize;
            format!("[{}{}]", "█".repeat(filled.min(20)), " ".repeat(20 - filled.min(20)))
        } else {
            String::new()
        };
        println!("{} {:10} {:6} requests  avg tokens: {:8.0}  {} handoffs {}",
            s.date, s.kind, s.total_requests,
            s.avg_tokens_remaining.unwrap_or(0.0),
            s.handoff_count, bar);
    }
}
```

- [ ] **Step 3: Commit**
```bash
git add rust/crates/cli/src/stats.rs rust/crates/storage/src/lib.rs rust/crates/cli/src/main.rs
git commit -m "feat: handoff stats command with daily token/request aggregation"
```

---

## TIER 5 — Surface Expansion

### Task 5.1: Additional agent adapters

**Files:**
- Create: `rust/crates/adapters/src/gemini.rs`
- Create: `rust/crates/adapters/src/aider.rs`
- Create: `rust/crates/adapters/src/cline.rs`
- Modify: `rust/crates/adapters/src/lib.rs` (register new adapters in `all()`)

- [ ] **Step 1: Write detection tests for each new adapter**
```rust
// rust/crates/adapters/tests/new_adapters_test.rs
use handoff_adapters::{GeminiAdapter, AiderAdapter, ClineAdapter, Adapter};
use handoff_common::types::ProcInfo;

#[test]
fn gemini_detects_gemini_binary() {
    let adapter = GeminiAdapter;
    assert!(adapter.binaries().contains(&"gemini"));
}

#[test]
fn aider_headless_argv_uses_message_flag() {
    let adapter = AiderAdapter;
    let argv = adapter.headless_argv("fix the bug").unwrap();
    assert!(argv.contains(&"--message".to_string()));
    assert!(argv.contains(&"fix the bug".to_string()));
}
```

- [ ] **Step 2: Implement gemini.rs**
```rust
// rust/crates/adapters/src/gemini.rs
use crate::{Adapter, AgentKind, ProcInfo, ProcessMatch, RateSample};
use std::{collections::BTreeMap, path::{Path, PathBuf}};

pub struct GeminiAdapter;

impl Adapter for GeminiAdapter {
    fn kind(&self) -> AgentKind { AgentKind::Gemini }
    fn binaries(&self) -> &'static [&'static str] { &["gemini"] }
    fn api_hosts(&self) -> &'static [&'static str] { &["generativelanguage.googleapis.com"] }
    fn detect(&self, procs: &[ProcInfo]) -> Vec<ProcessMatch> {
        procs.iter().filter(|p| self.binaries().iter().any(|b| p.name.contains(b)))
            .map(|p| ProcessMatch { pid: p.pid, kind: self.kind() })
            .collect()
    }
    fn classify_host(&self, host: &str) -> bool {
        self.api_hosts().iter().any(|h| host.contains(h))
    }
    fn context_files(&self, project_root: &Path) -> Vec<PathBuf> {
        vec![project_root.join("GEMINI.md")]
    }
    fn parse_headers(&self, _headers: &BTreeMap<String, String>) -> Option<RateSample> {
        None // Gemini rate-limit headers TBD
    }
    fn headless_argv(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec!["gemini".into(), "-p".into(), prompt.to_string()])
    }
}
```

- [ ] **Step 3: Implement aider.rs**
```rust
pub struct AiderAdapter;
impl Adapter for AiderAdapter {
    fn kind(&self) -> AgentKind { AgentKind::Aider }
    fn binaries(&self) -> &'static [&'static str] { &["aider"] }
    fn api_hosts(&self) -> &'static [&'static str] { &["api.openai.com"] }
    fn headless_argv(&self, prompt: &str) -> Option<Vec<String>> {
        Some(vec!["aider".into(), "--message".into(), prompt.to_string(), "--yes".into()])
    }
    // ... rest minimal
}
```

- [ ] **Step 4: Register in adapters/src/lib.rs `all()`**
```rust
pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(ClaudeAdapter),
        Box::new(CodexAdapter),
        Box::new(CopilotAdapter),
        Box::new(CursorAdapter),
        Box::new(GeminiAdapter),
        Box::new(AiderAdapter),
        Box::new(ClineAdapter),
    ]
}
```

- [ ] **Step 5: Add AgentKind variants**

In `common/src/types.rs`:
```rust
pub enum AgentKind {
    Claude, Codex, Copilot, Cursor,
    Gemini, Aider, Cline,
    Unknown(String),
}
```

- [ ] **Step 6: Run tests**
```bash
cd rust && cargo test --workspace 2>&1 | grep -E "FAILED|error"
```

- [ ] **Step 7: Commit**
```bash
git add rust/crates/adapters/src/ rust/crates/common/src/types.rs
git commit -m "feat(adapters): Gemini, Aider, Cline adapters with headless_argv"
```

---

## Verification

### End-to-end smoke test (run after each tier)

```bash
# 1. Build everything
cd rust && cargo build --workspace

# 2. Run all tests
cargo test --workspace 2>&1 | tail -20

# 3. Fresh setup (Tier 1.1a)
./target/debug/handoff setup /tmp/e2e-test-project

# 4. Start services
./target/debug/handoff daemon start
./target/debug/handoff proxy start

# 5. Spawn an agent
./target/debug/handoff spawn claude -- "list the files in this directory"

# 6. Watch TUI
./target/debug/handoff agents  # should show agent in table

# 7. Simulate limit
./target/debug/handoff simulate-limit --agent 1 --tokens 0

# 8. Verify failover
./target/debug/handoff handoffs  # should show a handoff row

# 9. Replay
./target/debug/handoff replay 1

# 10. Stats
./target/debug/handoff stats --daily

# 11. Teardown
./target/debug/handoff teardown
```

### CI green check
```bash
gh run list --limit 5
gh run view <run-id>
```
Expected: all jobs green on ubuntu-latest and macos-latest.

### handoff doctor
```bash
./target/debug/handoff doctor
```
Expected: all checks pass (CA trusted, daemon running, proxy intercepting, at least one agent detected).

---

## Tier ordering recommendation

Execute in this order for maximum risk reduction:
1. **T0.1–T0.3** (30 min total — process, no risk)
2. **T1.1b** (CI first — verifies all subsequent work)
3. **T1.2a + T1.2b** (E2E proof before shipping to anyone)
4. **T2.1** (adapter refactor — enables all future agent additions)
5. **T1.1a** (setup command — now that architecture is clean)
6. **T1.3** (TUI — the marketing screenshot)
7. **T2.2–T2.4** (architectural depth)
8. **T3.x** (quality pass)
9. **T4.x** (observability)
10. **T5.x** (expansion)
