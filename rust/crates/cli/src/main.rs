//! `handoff` CLI binary.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use handoff_adapters::{all as all_adapters, snapshot_procs};
use handoff_common::{db_path, AgentKind};
use handoff_context::{init_project, ContextEngine};
use handoff_storage::Database;

#[derive(Parser)]
#[command(name = "handoff", version, about = "multi-agent orchestration CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold .handoff/ in the project root.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Render brain.md into per-agent context files.
    Sync {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// List running agents with their latest rate-limit state.
    Agents {
        #[arg(long)]
        project_id: Option<i64>,
    },
    /// Scan running processes for known agents.
    Discover,
    /// Build an intelligent Snapshot of the current project state.
    Snapshot {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value = "manual")]
        reason: String,
        /// Print JSON instead of the rendered Markdown.
        #[arg(long)]
        json: bool,
    },
    /// Daemon control.
    #[command(subcommand)]
    Daemon(DaemonCmd),
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Run the daemon in the foreground.
    Run {
        #[arg(long, default_value = "127.0.0.1:7879")]
        addr: SocketAddr,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { path } => cmd_init(path),
        Cmd::Sync { path } => cmd_sync(path),
        Cmd::Agents { project_id } => cmd_agents(project_id),
        Cmd::Discover => cmd_discover(),
        Cmd::Snapshot { path, reason, json } => cmd_snapshot(path, reason, json),
        Cmd::Daemon(DaemonCmd::Run { addr }) => cmd_daemon_run(addr).await,
    }
}

fn cmd_init(path: PathBuf) -> Result<()> {
    let root = path.canonicalize().context("resolving project path")?;
    let h = init_project(&root)?;
    let db = Database::open(&db_path())?;
    let pid = db.upsert_project(&root.display().to_string())?;
    println!("initialized {} (project_id={pid})", h.display());
    println!("next: edit .handoff/brain.md and .handoff/intent.md, then run `handoff sync`");
    Ok(())
}

fn cmd_sync(path: PathBuf) -> Result<()> {
    let root = path.canonicalize()?;
    let written = ContextEngine::new(&root).sync()?;
    for p in written {
        println!("wrote {}", p.display());
    }
    Ok(())
}

fn cmd_agents(project_id: Option<i64>) -> Result<()> {
    let db = Database::open(&db_path())?;
    let agents = db.list_agent_summaries(project_id)?;
    if agents.is_empty() {
        println!("no agents tracked yet");
        return Ok(());
    }
    println!(
        "{:>4}  {:<10}  {:>6}  {:<10}  {:>10}  {:>10}  {:>9}",
        "id", "kind", "pid", "status", "tok_rem", "req_rem", "last"
    );
    let now = Utc::now().timestamp();
    for a in agents {
        let last = a
            .last_sample_ts
            .map(|t| format!("{}s", now - t))
            .unwrap_or_else(|| "-".into());
        println!(
            "{:>4}  {:<10}  {:>6}  {:<10}  {:>10}  {:>10}  {:>9}",
            a.id,
            a.kind,
            a.pid.map(|x| x.to_string()).unwrap_or_else(|| "-".into()),
            a.status,
            a.tokens_remaining
                .map(|x| x.to_string())
                .unwrap_or_else(|| "-".into()),
            a.requests_remaining
                .map(|x| x.to_string())
                .unwrap_or_else(|| "-".into()),
            last,
        );
    }
    Ok(())
}

fn cmd_discover() -> Result<()> {
    let procs = snapshot_procs();
    let mut found = 0;
    for adapter in all_adapters() {
        for m in adapter.detect(&procs) {
            let cmd = m.cmdline.join(" ");
            let truncated: String = cmd.chars().take(80).collect();
            println!(
                "{:<10}  pid={:<6}  {}",
                adapter.kind().as_str(),
                m.pid,
                truncated
            );
            found += 1;
        }
    }
    if found == 0 {
        println!("no known agents running");
    }
    Ok(())
}

fn cmd_snapshot(path: PathBuf, reason: String, json: bool) -> Result<()> {
    let root = path.canonicalize()?;
    let (snap, md_path) = ContextEngine::new(&root).snapshot(&reason)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snap)?);
    } else {
        println!("{}", handoff_context::render_markdown(&snap));
        println!("\n→ wrote {}", md_path.display());
    }
    Ok(())
}

async fn cmd_daemon_run(addr: SocketAddr) -> Result<()> {
    let db = Arc::new(Database::open(&db_path())?);
    let state = handoff_daemon::AppState { db };
    println!("handoffd listening on {}", addr);
    handoff_daemon::serve(state, addr).await
}

/// Suppress unused-import warning for AgentKind which will be used by
/// upcoming attach/spawn commands.
#[allow(dead_code)]
fn _ak_keepalive() -> Option<AgentKind> {
    AgentKind::parse("claude")
}
