//! `handoff` CLI binary.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use handoff_adapters::{all as all_adapters, snapshot_procs};
use handoff_common::{
    daemon_pidfile, db_path, home_dir, proxy_pidfile, AgentKind,
};
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
        #[arg(long)]
        json: bool,
    },
    /// Spawn an agent with HTTPS_PROXY wired and register it with the daemon.
    Spawn {
        kind: String,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        no_proxy: bool,
    },
    /// Register an already-running agent process with the daemon.
    Attach {
        pid: i64,
        #[arg(long)]
        kind: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Manual handoff: snapshot context and (optionally) spawn the next agent.
    Handoff {
        to_kind: String,
        #[arg(long = "from")]
        from_agent: Option<i64>,
        #[arg(long, default_value = "manual")]
        reason: String,
        #[arg(long)]
        no_spawn: bool,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// View / edit the project brain.
    #[command(subcommand)]
    Brain(BrainCmd),
    /// Cheap-worker / expensive-critic loop.
    #[command(subcommand)]
    Critic(CriticCmd),
    /// Daemon control.
    #[command(subcommand)]
    Daemon(DaemonCmd),
    /// Local MITM proxy control.
    #[command(subcommand)]
    Proxy(ProxyCmd),
    /// Internal: run the proxy server in the foreground. Used by `proxy start`.
    #[command(name = "_proxy_server", hide = true)]
    ProxyServer {
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
    },
}

#[derive(Subcommand)]
enum BrainCmd {
    Cat {
        #[arg(default_value = ".")]
        project: PathBuf,
    },
    Edit {
        #[arg(default_value = ".")]
        project: PathBuf,
    },
    Append {
        text: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Subcommand)]
enum CriticCmd {
    /// One-shot worker+critic run.
    Run {
        task: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, default_value = handoff_critic::DEFAULT_WORKER_MODEL)]
        worker_model: String,
        #[arg(long, default_value = handoff_critic::DEFAULT_CRITIC_MODEL)]
        critic_model: String,
        #[arg(long)]
        no_proxy: bool,
    },
    /// Re-run the critic loop whenever tracked files change.
    Watch {
        task: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, default_value = handoff_critic::DEFAULT_WORKER_MODEL)]
        worker_model: String,
        #[arg(long, default_value = handoff_critic::DEFAULT_CRITIC_MODEL)]
        critic_model: String,
        #[arg(long, default_value_t = 2.0)]
        interval: f64,
        #[arg(long, default_value_t = 3.0)]
        debounce: f64,
        #[arg(long)]
        no_proxy: bool,
    },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Run the daemon in the foreground.
    Run {
        #[arg(long, default_value = "127.0.0.1:7879")]
        addr: SocketAddr,
    },
    /// Spawn the daemon as a background process.
    Start {
        #[arg(long, default_value = "127.0.0.1:7879")]
        addr: SocketAddr,
    },
    Stop,
    Status,
}

#[derive(Subcommand)]
enum ProxyCmd {
    /// Start the local MITM proxy as a background process.
    Start {
        #[arg(long, default_value = "127.0.0.1:8080")]
        addr: SocketAddr,
    },
    Stop,
    Status,
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
        Cmd::Spawn { kind, args, project, no_proxy } => {
            cmd_spawn(&kind, args, project, no_proxy).await
        }
        Cmd::Attach { pid, kind, project } => cmd_attach(pid, kind, project).await,
        Cmd::Handoff { to_kind, from_agent, reason, no_spawn, project } => {
            cmd_handoff(to_kind, from_agent, reason, !no_spawn, project).await
        }
        Cmd::Brain(BrainCmd::Cat { project }) => cmd_brain_cat(project),
        Cmd::Brain(BrainCmd::Edit { project }) => cmd_brain_edit(project),
        Cmd::Brain(BrainCmd::Append { text, project }) => cmd_brain_append(text, project),
        Cmd::Critic(CriticCmd::Run { task, project, worker_model, critic_model, no_proxy }) => {
            cmd_critic_run(&task, project, worker_model, critic_model, no_proxy).await
        }
        Cmd::Critic(CriticCmd::Watch {
            task, project, worker_model, critic_model, interval, debounce, no_proxy,
        }) => cmd_critic_watch(task, project, worker_model, critic_model, interval, debounce, no_proxy).await,
        Cmd::Daemon(DaemonCmd::Run { addr }) => cmd_daemon_run(addr).await,
        Cmd::Daemon(DaemonCmd::Start { addr }) => cmd_daemon_start(addr),
        Cmd::Daemon(DaemonCmd::Stop) => cmd_daemon_stop(),
        Cmd::Daemon(DaemonCmd::Status) => cmd_daemon_status().await,
        Cmd::Proxy(ProxyCmd::Start { addr }) => cmd_proxy_start(addr),
        Cmd::Proxy(ProxyCmd::Stop) => cmd_proxy_stop(),
        Cmd::Proxy(ProxyCmd::Status) => cmd_proxy_status(),
        Cmd::ProxyServer { addr } => handoff_proxy::run(addr, None).await,
    }
}

// --- core ---------------------------------------------------------------

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

// --- spawn / attach / handoff -------------------------------------------

async fn cmd_spawn(kind: &str, args: Vec<String>, project: PathBuf, no_proxy: bool) -> Result<()> {
    let _ak = AgentKind::parse(kind).ok_or_else(|| anyhow!("unknown kind: {kind}"))?;
    let adapter = handoff_adapters::for_kind(_ak);
    let project_root = project.canonicalize()?;
    let bin = adapter
        .binaries()
        .iter()
        .find_map(|b| which::which(b).ok())
        .ok_or_else(|| anyhow!("no binary on PATH for kind={kind}"))?;
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(&args).current_dir(&project_root);
    if !no_proxy {
        for (k, v) in handoff_daemon::spawn::proxy_env(&proxy_url()) {
            cmd.env(k, v);
        }
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    rpc_call(
        "register_project",
        serde_json::json!({"root": project_root.display().to_string()}),
    )
    .await?;
    let res = rpc_call(
        "register_agent",
        serde_json::json!({"kind": kind, "pid": pid, "spawned_by": "handoff"}),
    )
    .await?;
    let aid = res
        .get("agent_id")
        .and_then(|v| v.as_i64())
        .unwrap_or_default();
    println!("spawned {kind} pid={pid} agent_id={aid}");
    let status = child.wait()?;
    let _ = rpc_call(
        "stop_agent",
        serde_json::json!({"agent_id": aid, "status": if status.success() {"stopped"} else {"failed"}}),
    )
    .await;
    Ok(())
}

async fn cmd_attach(pid: i64, kind: String, project: PathBuf) -> Result<()> {
    let project_root = project.canonicalize()?;
    rpc_call(
        "register_project",
        serde_json::json!({"root": project_root.display().to_string()}),
    )
    .await?;
    let res = rpc_call(
        "attach_agent",
        serde_json::json!({"kind": kind, "pid": pid}),
    )
    .await?;
    let aid = res.get("agent_id").and_then(|v| v.as_i64()).unwrap_or(0);
    println!("attached {kind} pid={pid} agent_id={aid}");
    Ok(())
}

async fn cmd_handoff(
    to_kind: String,
    from_agent: Option<i64>,
    reason: String,
    auto_spawn: bool,
    project: PathBuf,
) -> Result<()> {
    let project_root = project.canonicalize()?;
    rpc_call(
        "register_project",
        serde_json::json!({"root": project_root.display().to_string()}),
    )
    .await?;
    let res = rpc_call(
        "handoff",
        serde_json::json!({
            "to_kind": to_kind,
            "from_agent_id": from_agent,
            "reason": reason,
            "auto_spawn": auto_spawn,
        }),
    )
    .await?;
    println!("handoff -> {to_kind}");
    if let Some(p) = res.get("snapshot_path").and_then(|v| v.as_str()) {
        println!("  snapshot: {p}");
    }
    if let Some(aid) = res.get("to_agent_id").and_then(|v| v.as_i64()) {
        println!(
            "  spawned agent_id={aid} pid={}",
            res.get("to_pid").map(|v| v.to_string()).unwrap_or_default()
        );
    } else {
        println!("  no agent spawned");
    }
    Ok(())
}

// --- brain --------------------------------------------------------------

fn brain_path(project: &PathBuf) -> Result<PathBuf> {
    let p = project.canonicalize()?.join(".handoff").join("brain.md");
    if !p.exists() {
        return Err(anyhow!("no brain at {} — run `handoff init`", p.display()));
    }
    Ok(p)
}

fn cmd_brain_cat(project: PathBuf) -> Result<()> {
    let p = brain_path(&project)?;
    let body = std::fs::read_to_string(p)?;
    print!("{body}");
    Ok(())
}

fn cmd_brain_edit(project: PathBuf) -> Result<()> {
    let p = brain_path(&project)?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(&editor).arg(&p).status()?;
    if !status.success() {
        return Err(anyhow!("$EDITOR exited with {status}"));
    }
    Ok(())
}

fn cmd_brain_append(text: String, project: PathBuf) -> Result<()> {
    let p = brain_path(&project)?;
    let mut existing = std::fs::read_to_string(&p)?;
    if !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str(text.trim_end());
    existing.push('\n');
    std::fs::write(&p, existing)?;
    println!("appended to {}", p.display());
    Ok(())
}

// --- critic -------------------------------------------------------------

async fn cmd_critic_run(
    task: &str,
    project: PathBuf,
    worker: String,
    critic: String,
    no_proxy: bool,
) -> Result<()> {
    let root = project.canonicalize()?;
    let runner = handoff_critic::CriticRunner::new(&root)?
        .with_models(worker.clone(), critic.clone())
        .with_proxy(if no_proxy { None } else { Some(proxy_url()) });
    let res = runner.run(task).await?;
    println!("verdict: {}", res.verdict);
    println!("notes: {}", res.notes);
    println!("tokens: worker={} critic={}", res.worker_tokens, res.critic_tokens);
    for a in &res.artifacts {
        println!("  wrote {a}");
    }
    let _ = rpc_call(
        "record_critic_run",
        serde_json::json!({
            "worker_model": worker,
            "critic_model": critic,
            "worker_tokens": res.worker_tokens,
            "critic_tokens": res.critic_tokens,
            "verdict": res.verdict,
            "notes": res.notes,
        }),
    )
    .await;
    Ok(())
}

async fn cmd_critic_watch(
    task: String,
    project: PathBuf,
    worker: String,
    critic: String,
    interval: f64,
    debounce: f64,
    no_proxy: bool,
) -> Result<()> {
    let root = project.canonicalize()?;
    let runner = handoff_critic::CriticRunner::new(&root)?
        .with_models(worker.clone(), critic.clone())
        .with_proxy(if no_proxy { None } else { Some(proxy_url()) });
    let mut watch = handoff_critic::watch::WatchLoop::new(&root, interval, debounce);
    println!("watching {} (Ctrl-C to stop)", root.display());
    let interval_dur = Duration::from_secs_f64(interval.max(0.05));
    loop {
        let tick = watch.tick_now();
        if tick.fired {
            println!("tracked files changed → running critic");
            match runner.run(&task).await {
                Ok(res) => {
                    println!(
                        "  {} worker={} critic={}",
                        res.verdict, res.worker_tokens, res.critic_tokens,
                    );
                    let _ = rpc_call(
                        "record_critic_run",
                        serde_json::json!({
                            "worker_model": worker.clone(),
                            "critic_model": critic.clone(),
                            "worker_tokens": res.worker_tokens,
                            "critic_tokens": res.critic_tokens,
                            "verdict": res.verdict,
                            "notes": res.notes,
                        }),
                    )
                    .await;
                }
                Err(e) => println!("  critic run failed: {e}"),
            }
        }
        tokio::time::sleep(interval_dur).await;
    }
}

// --- daemon -------------------------------------------------------------

async fn cmd_daemon_run(addr: SocketAddr) -> Result<()> {
    let db = Arc::new(Database::open(&db_path())?);
    let state = handoff_daemon::AppState::bootstrap(db, proxy_url());
    println!("handoffd listening on {}", addr);
    handoff_daemon::serve(state, addr).await
}

fn cmd_daemon_start(addr: SocketAddr) -> Result<()> {
    let pidfile = daemon_pidfile();
    if let Ok(s) = std::fs::read_to_string(&pidfile) {
        if let Ok(pid) = s.trim().parse::<i32>() {
            if pid_alive(pid) {
                println!("daemon already running (pid={pid})");
                return Ok(());
            }
        }
    }
    let log = home_dir().join("daemon.log");
    let log_fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    let log_err = log_fh.try_clone()?;
    let me = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&me);
    cmd.args(["daemon", "run", "--addr", &addr.to_string()])
        .stdout(Stdio::from(log_fh))
        .stderr(Stdio::from(log_err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }
    let child = cmd.spawn()?;
    std::fs::write(&pidfile, child.id().to_string())?;
    println!("daemon up pid={} log={}", child.id(), log.display());
    Ok(())
}

#[cfg(unix)]
fn libc_setsid() {
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe {
        setsid();
    }
}

#[cfg(not(unix))]
fn libc_setsid() {}

fn cmd_daemon_stop() -> Result<()> {
    stop_pidfile(&daemon_pidfile())
}

async fn cmd_daemon_status() -> Result<()> {
    match reqwest::Client::new()
        .get(format!("http://127.0.0.1:7879/health"))
        .timeout(Duration::from_secs(1))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            println!("up {}", r.text().await.unwrap_or_default());
        }
        _ => println!("down"),
    }
    Ok(())
}

// --- proxy --------------------------------------------------------------

fn cmd_proxy_start(addr: SocketAddr) -> Result<()> {
    let pidfile = proxy_pidfile();
    if let Ok(s) = std::fs::read_to_string(&pidfile) {
        if let Ok(pid) = s.trim().parse::<i32>() {
            if pid_alive(pid) {
                println!("proxy already running (pid={pid})");
                return Ok(());
            }
        }
    }
    let log = home_dir().join("proxy.log");
    let log_fh = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    let log_err = log_fh.try_clone()?;
    let me = std::env::current_exe()?;
    // Implementation note: the proxy is its own subcommand here — but to
    // keep all binaries in one CLI we shell out to the same `handoff`
    // binary with a hidden `_proxy_server` flag once that's wired. For now
    // we exec the standalone `handoff-proxy-server` if present, else fall
    // back to the embedded server entry.
    let mut cmd = std::process::Command::new(&me);
    cmd.args(["_proxy_server", "--addr", &addr.to_string()])
        .stdout(Stdio::from(log_fh))
        .stderr(Stdio::from(log_err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc_setsid();
                Ok(())
            });
        }
    }
    let child = cmd.spawn()?;
    std::fs::write(&pidfile, child.id().to_string())?;
    println!("proxy up pid={} {}", child.id(), addr);
    let ca = home_dir().join("ca").join("cert.pem");
    if !ca.exists() {
        println!(
            "first run: install the CA so spawned agents trust HTTPS:\n  \
             linux:   sudo cp {0} /usr/local/share/ca-certificates/handoff.crt && \
             sudo update-ca-certificates\n  \
             macOS:   sudo security add-trusted-cert -d -r trustRoot \
             -k /Library/Keychains/System.keychain {0}",
            ca.display()
        );
    }
    Ok(())
}

fn cmd_proxy_stop() -> Result<()> {
    stop_pidfile(&proxy_pidfile())
}

fn cmd_proxy_status() -> Result<()> {
    let pidfile = proxy_pidfile();
    let Ok(s) = std::fs::read_to_string(&pidfile) else {
        println!("down");
        return Ok(());
    };
    let Ok(pid) = s.trim().parse::<i32>() else {
        println!("down (bad pidfile)");
        return Ok(());
    };
    if pid_alive(pid) {
        println!("up pid={pid}");
    } else {
        println!("down (stale pidfile pid={pid})");
    }
    Ok(())
}

// --- helpers ------------------------------------------------------------

fn proxy_url() -> String {
    std::env::var("HANDOFF_PROXY_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into())
}

fn pid_alive(pid: i32) -> bool {
    #[cfg(unix)]
    unsafe {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        kill(pid, 0) == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn stop_pidfile(pidfile: &std::path::Path) -> Result<()> {
    let Ok(s) = std::fs::read_to_string(pidfile) else {
        println!("no pidfile");
        return Ok(());
    };
    let Ok(pid) = s.trim().parse::<i32>() else {
        let _ = std::fs::remove_file(pidfile);
        return Ok(());
    };
    #[cfg(unix)]
    unsafe {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        kill(pid, 15);
    }
    let _ = std::fs::remove_file(pidfile);
    println!("sent SIGTERM to {pid}");
    Ok(())
}

async fn rpc_call(method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let url = std::env::var("HANDOFF_DAEMON_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:7879/rpc".into());
    let body = serde_json::json!({"method": method, "params": params});
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .with_context(|| {
            format!("cannot reach daemon at {url}; run `handoff daemon start` first")
        })?;
    let v: serde_json::Value = resp.json().await?;
    if !v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
        let err = v
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("(unknown rpc error)");
        return Err(anyhow!("rpc {method}: {err}"));
    }
    Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

#[allow(dead_code)]
fn _ak_keepalive() -> Option<AgentKind> {
    AgentKind::parse("claude")
}
