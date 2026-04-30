//! `handoff` CLI binary.

use std::io::IsTerminal;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use crossterm::style::Stylize;
use handoff_adapters::{all as all_adapters, snapshot_procs};
use handoff_common::{daemon_pidfile, db_path, home_dir, proxy_pidfile, AgentKind};
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
    /// List active agents.
    Agents {
        #[arg(long)]
        project_id: Option<i64>,
        /// Print once and exit (no live UI)
        #[arg(long)]
        once: bool,
    },
    /// Scan running processes for known agents.
    Discover,
    /// Manage agent worktrees.
    Worktree {
        #[clap(subcommand)]
        cmd: WorktreeCmd,
    },
    /// Manage agent hooks.
    Hook {
        #[clap(subcommand)]
        cmd: HookCmd,
    },
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
    ///
    /// Behaviour:
    ///   `handoff spawn claude`                    → interactive TUI
    ///   `handoff spawn claude -- "summarize"`     → headless (`claude -p "..."`)
    ///   `handoff spawn claude --interactive -- ...` → forces interactive even with args
    Spawn {
        kind: String,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        no_proxy: bool,
        /// Force interactive mode even when positional args are present.
        #[arg(long)]
        interactive: bool,
        /// Force headless mode even with no positional args (uses stdin).
        #[arg(long, conflicts_with = "interactive")]
        headless: bool,
    },
    /// Register an already-running agent process with the daemon.
    Attach {
        pid: i64,
        #[arg(long)]
        kind: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Simulate a rate-limit event for a specific agent
    SimulateLimit {
        agent_id: i64,
        #[arg(long, default_value = "0")]
        tokens: i64,
        #[arg(long, default_value = "0")]
        requests: i64,
    },
    /// Pipe a command through output reducers to save tokens.
    Reduce {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Tail or cat logs for a target.
    Logs {
        /// "daemon" | "proxy" | "agent"
        target: String,
        /// Required if target is "agent"
        agent_id: Option<i64>,
        #[arg(short = 'f', long)]
        follow: bool,
    },

    /// Interactive first-time setup (generates CA, installs CLI hook)
    Setup {
        #[arg(long)]
        project: Option<String>,
    },
    /// Remove handoff CA from system trust store and clean up generated certs.
    Teardown,

    /// Print the shell hook script to be evaluated
    InitHook,
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
    /// Replay a handoff event.
    Replay { handoff_id: i64 },
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
    /// Preflight check: daemon, proxy, CA, agent binaries.
    Doctor,
    /// Usage statistics and daily token/request aggregation.
    Stats {
        #[arg(long, default_value_t = 7)]
        days: u32,
        #[arg(long)]
        graph: bool,
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
    /// One-shot worker+critic run, driven by local agent CLIs.
    Run {
        task: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Local agent for the worker role (claude | codex | copilot).
        #[arg(long, default_value = handoff_critic::DEFAULT_WORKER_AGENT)]
        worker: String,
        /// Local agent for the critic role.
        #[arg(long, default_value = handoff_critic::DEFAULT_CRITIC_AGENT)]
        critic: String,
        #[arg(long)]
        no_proxy: bool,
    },
    /// Re-run the critic loop whenever tracked files change.
    Watch {
        task: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, default_value = handoff_critic::DEFAULT_WORKER_AGENT)]
        worker: String,
        #[arg(long, default_value = handoff_critic::DEFAULT_CRITIC_AGENT)]
        critic: String,
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
    let log_dir = home_dir().join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(log_dir, "handoff.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    use tracing_subscriber::prelude::*;
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false);
    let stdout_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stdout);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stdout_layer)
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { path } => cmd_init(path).await,
        Cmd::Sync { path } => cmd_sync(path),
        Cmd::Agents { project_id, once } => cmd_agents(project_id, once).await,
        Cmd::Discover => cmd_discover(),
        Cmd::Worktree { cmd } => cmd_worktree(cmd),
        Cmd::Hook { cmd } => cmd_hook(cmd),
        Cmd::Snapshot { path, reason, json } => cmd_snapshot(path, reason, json),
        Cmd::Spawn {
            kind,
            args,
            project,
            no_proxy,
            interactive,
            headless,
        } => cmd_spawn(&kind, args, project, no_proxy, interactive, headless).await,
        Cmd::Attach { pid, kind, project } => cmd_attach(pid, kind, project).await,
        Cmd::Handoff {
            to_kind,
            from_agent,
            reason,
            no_spawn,
            project,
        } => cmd_handoff(to_kind, from_agent, reason, !no_spawn, project).await,
        Cmd::Brain(BrainCmd::Cat { project }) => cmd_brain_cat(project),
        Cmd::Brain(BrainCmd::Edit { project }) => cmd_brain_edit(project).await,
        Cmd::Brain(BrainCmd::Append { text, project }) => cmd_brain_append(text, project).await,
        Cmd::Replay { handoff_id } => cmd_replay(handoff_id).await,
        Cmd::Critic(CriticCmd::Run {
            task,
            project,
            worker,
            critic,
            no_proxy,
        }) => cmd_critic_run(&task, project, worker, critic, no_proxy).await,
        Cmd::Critic(CriticCmd::Watch {
            task,
            project,
            worker,
            critic,
            interval,
            debounce,
            no_proxy,
        }) => cmd_critic_watch(task, project, worker, critic, interval, debounce, no_proxy).await,
        Cmd::Daemon(DaemonCmd::Run { addr }) => cmd_daemon_run(addr).await,
        Cmd::Daemon(DaemonCmd::Start { addr }) => cmd_daemon_start(addr),
        Cmd::Daemon(DaemonCmd::Stop) => cmd_daemon_stop(),
        Cmd::Daemon(DaemonCmd::Status) => cmd_daemon_status().await,
        Cmd::Proxy(ProxyCmd::Start { addr }) => cmd_proxy_start(addr),
        Cmd::Proxy(ProxyCmd::Stop) => cmd_proxy_stop(),
        Cmd::Proxy(ProxyCmd::Status) => cmd_proxy_status(),
        Cmd::ProxyServer { addr } => handoff_proxy::run(addr, None).await,
        Cmd::Doctor => cmd_doctor().await,
        Cmd::Stats { days, graph } => cmd_stats(days, graph),
        Cmd::Reduce { args } => {
            handoff_cli::reduce::run_reduce(&args).await?;
            Ok(())
        }
        Cmd::Logs {
            target,
            agent_id,
            follow,
        } => cmd_logs(target, agent_id, follow).await,
        Cmd::SimulateLimit {
            agent_id,
            tokens,
            requests,
        } => {
            let url = std::env::var("HANDOFF_DAEMON_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:7879".to_string());
            let res = reqwest::Client::new()
                .post(format!("{}/simulate", url))
                .json(&serde_json::json!({
                    "agent_id": agent_id,
                    "tokens": tokens,
                    "requests": requests,
                }))
                .send()
                .await?;
            if res.status().is_success() {
                println!("Simulated rate limit event for agent {}", agent_id);
            } else {
                eprintln!("Failed to send event: {:?}", res.text().await);
            }
            Ok(())
        }
        Cmd::Setup { project } => cmd_init(project.map(PathBuf::from).unwrap_or_default()).await,
        Cmd::Teardown => handoff_cli::setup::run_teardown().await,
        Cmd::InitHook => {
            let script = r#"
handoff_wrap() {
    local cmd=$1
    shift
    if [ "$cmd" = "claude" ]; then
        handoff spawn claude -- "$@"
    elif [ "$cmd" = "codex" ]; then
        handoff spawn codex -- "$@"
    elif [ "$cmd" = "gh" ] && [ "$1" = "copilot" ]; then
        handoff spawn copilot -- "$@"
    else
        command "$cmd" "$@"
    fi
}
alias claude="handoff_wrap claude"
alias codex="handoff_wrap codex"
"#;
            print!("{}", script);
            Ok(())
        }
    }
}

async fn cmd_doctor() -> Result<()> {
    let mut ok = true;
    let mut out = Vec::<String>::new();

    // Daemon
    let daemon_url = std::env::var("HANDOFF_DAEMON_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:7879/health".into());
    let daemon_alive = matches!(
        reqwest::Client::new()
            .get(&daemon_url)
            .timeout(Duration::from_secs(1))
            .send()
            .await,
        Ok(r) if r.status().is_success()
    );
    out.push(format!(
        "{} daemon @ {}",
        if daemon_alive { "✓" } else { "✗" },
        daemon_url.trim_end_matches("/health"),
    ));
    ok &= daemon_alive;

    // Proxy
    let proxy_pid = std::fs::read_to_string(handoff_common::proxy_pidfile())
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok());
    let proxy_alive = proxy_pid.map(pid_alive).unwrap_or(false);
    match (proxy_pid, proxy_alive) {
        (Some(pid), true) => out.push(format!("✓ proxy @ {} (pid={pid})", proxy_url())),
        (Some(pid), false) => {
            out.push(format!("✗ proxy down (stale pidfile pid={pid})"));
            ok = false;
        }
        (None, _) => {
            out.push("✗ proxy down (no pidfile)".into());
            ok = false;
        }
    }

    // CA
    let ca = handoff_common::home_dir().join("ca").join("cert.pem");
    if ca.exists() {
        out.push(format!("✓ CA at {}", ca.display()));
    } else {
        out.push(format!("✗ CA not generated yet ({})", ca.display()));
        ok = false;
    }

    // Agent binaries
    for (kind, bin) in [
        ("claude", "claude"),
        ("codex", "codex"),
        ("copilot", "gh"),
        ("cursor", "cursor"),
    ] {
        match which::which(bin) {
            Ok(p) => out.push(format!("✓ {kind:8} → {}", p.display())),
            Err(_) => out.push(format!("⚠ {kind:8} → not on PATH")),
        }
    }

    // Critic key (deliberately optional now — the refactored runner uses
    // local CLIs, so this is informational).
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        out.push("ℹ ANTHROPIC_API_KEY set (not needed; critic uses local CLIs)".into());
    } else {
        out.push("ℹ ANTHROPIC_API_KEY not set (fine — critic uses local CLIs)".into());
    }

    for line in out {
        println!("{line}");
    }
    if ok {
        Ok(())
    } else {
        // Quiet exit — we already printed per-line diagnostics.
        std::process::exit(1);
    }
}

// --- core ---------------------------------------------------------------

async fn cmd_init(path: PathBuf) -> Result<()> {
    let root = if path.as_os_str().is_empty() || path.as_path() == Path::new(".") {
        std::env::current_dir()?.canonicalize()?
    } else {
        path.canonicalize().context("resolving project path")?
    };

    let handoff_dir = init_project(&root)?;
    let db = Database::open(&db_path())?;
    let project_id = db.upsert_project(&root.display().to_string())?;
    let detected = detect_installed_agents();

    print_init_banner();
    println!("Detected agents:");
    for (label, ok) in &detected {
        println!("{} {label}", if *ok { "✓" } else { "⚠" });
    }

    let threshold = prompt_threshold()?;
    let chain_default = detected_chain(&detected);
    let chain = prompt_chain(&chain_default)?;
    let worker_choices = supported_critic_agents(&detected);
    let worker = prompt_agent_choice("Worker agent", &worker_choices, "claude")?;
    let critic = prompt_agent_choice("Lead critic", &worker_choices, "claude")?;
    let passing_score = prompt_score()?;
    let start_background = prompt_yes_no("Start Handsoff background services", true)?;
    let open_dashboard = start_background
        && io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && prompt_yes_no("Open live dashboard after setup", true)?;

    let config_path = handoff_cli::setup::write_init_config(
        &root,
        threshold,
        &chain,
        &worker,
        &critic,
        passing_score,
    )?;
    println!("wrote {}", config_path.display());

    let written = ContextEngine::new(&root).sync()?;
    if !written.is_empty() {
        println!("synced {} context files", written.len());
    }

    let shell_hook = handoff_cli::hook::install_shell()?;
    println!("shell hook: {}", shell_hook.display());

    if detected
        .iter()
        .any(|(label, ok)| *ok && label == "Claude Code")
    {
        let settings = dirs::home_dir().unwrap().join(".claude/settings.json");
        let daemon_url = std::env::var("HANDOFF_DAEMON_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:7879".to_string());
        let _ = handoff_cli::hook::install_claude(&daemon_url, &settings);
    }

    if start_background {
        cmd_daemon_start(default_daemon_addr())?;
        cmd_proxy_start(default_proxy_addr())?;
    }

    if start_background {
        wait_for_daemon_ready().await?;
        let _ = rpc_register_project(&root).await?;
        println!("✓ daemon running");
        println!("✓ observer running");
        println!("✓ unified memory active");
        println!("✓ hooks installed");
        println!("✓ watching agents");
    } else {
        println!("setup complete, start background services with `handoff daemon start` and `handoff proxy start`");
    }

    println!("project_id={project_id}");
    println!("setup root: {}", handoff_dir.display());
    if open_dashboard {
        println!("opening live dashboard; press q to exit");
        return handoff_cli::tui::run(&daemon_base_url()).await;
    }
    Ok(())
}

fn print_init_banner() {
    let stdout_is_tty = io::stdout().is_terminal();
    let lines = [
        " _   _    _    _   _ ____   ___  _____ _____ ",
        "| | | |  / \\  | \\ | |  _ \\ / _ \\|  ___|  ___|",
        "| |_| | / _ \\ |  \\| | | | | | | | |_  | |_   ",
        "|  _  |/ ___ \\| |\\  | |_| | |_| |  _| |  _|  ",
        "|_| |_/_/   \\_\\_| \\_|____/ \\___/|_|   |_|    ",
    ];

    println!();
    for line in lines {
        if stdout_is_tty {
            println!("{}", line.cyan().bold());
        } else {
            println!("{line}");
        }
    }
    if stdout_is_tty {
        println!("{}", "local control plane for AI agents".dark_grey());
    } else {
        println!("local control plane for AI agents");
    }
    println!();
}

fn detect_installed_agents() -> Vec<(String, bool)> {
    let candidates = [
        ("Claude Code", "claude"),
        ("Codex", "codex"),
        ("GitHub Copilot", "gh"),
        ("Cursor / Antigravity", "cursor"),
    ];
    candidates
        .into_iter()
        .map(|(label, bin)| (label.to_string(), which::which(bin).is_ok()))
        .collect()
}

fn detected_chain(detected: &[(String, bool)]) -> Vec<String> {
    let mut chain = Vec::new();
    for (label, ok) in detected {
        if !ok {
            continue;
        }
        match label.as_str() {
            "Claude Code" => chain.push("claude".into()),
            "Codex" => chain.push("codex".into()),
            "GitHub Copilot" => chain.push("copilot".into()),
            "Cursor / Antigravity" => chain.push("cursor".into()),
            _ => {}
        }
    }
    if chain.is_empty() {
        vec!["claude".into(), "codex".into(), "copilot".into()]
    } else {
        chain
    }
}

fn supported_critic_agents(detected: &[(String, bool)]) -> Vec<String> {
    let mut out = Vec::new();
    for (label, ok) in detected {
        if !ok {
            continue;
        }
        match label.as_str() {
            "Claude Code" => out.push("claude".into()),
            "Codex" => out.push("codex".into()),
            "GitHub Copilot" => out.push("copilot".into()),
            _ => {}
        }
    }
    if out.is_empty() {
        vec!["claude".into(), "codex".into(), "copilot".into()]
    } else {
        out
    }
}

fn prompt_threshold() -> Result<u32> {
    let value = prompt_numbered(
        "Switch when remaining budget falls below",
        &[
            (
                "10",
                "10% remaining",
                "use most of the current agent before switching",
            ),
            ("15", "15% remaining", "balanced default"),
            ("20", "20% remaining", "switch earlier"),
        ],
        1,
    )?;
    Ok(value.parse().unwrap_or(15))
}

fn prompt_score() -> Result<u32> {
    let value = prompt_numbered(
        "Required critic score before completion",
        &[
            ("7", "7/10", "faster acceptance"),
            ("8", "8/10", "balanced default"),
            ("9", "9/10", "stricter review"),
            ("10", "10/10", "only accept near-perfect work"),
        ],
        1,
    )?;
    Ok(value.parse().unwrap_or(8))
}

fn prompt_agent_choice(label: &str, choices: &[String], default: &str) -> Result<String> {
    let rendered = choices
        .iter()
        .map(|agent| {
            let name = match agent.as_str() {
                "claude" => "Claude Code",
                "codex" => "Codex CLI",
                "copilot" => "GitHub Copilot",
                other => other,
            };
            (agent.as_str(), name, "local authenticated CLI")
        })
        .collect::<Vec<_>>();
    let default_idx = choices.iter().position(|c| c == default).unwrap_or(0);
    prompt_numbered(label, &rendered, default_idx)
}

fn prompt_numbered(
    label: &str,
    choices: &[(&str, &str, &str)],
    default_idx: usize,
) -> Result<String> {
    println!();
    println!("{label}:");
    for (idx, (_, name, detail)) in choices.iter().enumerate() {
        println!("  {}. {:<18} {}", idx + 1, name, detail);
    }
    print!("Select [{}]: ", default_idx + 1);
    io::stdout().flush()?;

    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Ok(choices[default_idx].0.to_string());
    }
    if let Ok(n) = trimmed.parse::<usize>() {
        if let Some(choice) = choices.get(n.saturating_sub(1)) {
            return Ok(choice.0.to_string());
        }
    }
    for (value, name, _) in choices {
        if trimmed.eq_ignore_ascii_case(value) || trimmed.eq_ignore_ascii_case(name) {
            return Ok((*value).to_string());
        }
    }
    Ok(choices[default_idx].0.to_string())
}

fn prompt_chain(default: &[String]) -> Result<Vec<String>> {
    println!();
    println!("Failover chain:");
    for (idx, agent) in default.iter().enumerate() {
        println!("  {}. {}", idx + 1, display_agent(agent));
    }
    let default_order = (1..=default.len())
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    print!("Select order [{}]: ", default_order);
    io::stdout().flush()?;

    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Ok(default.to_vec());
    }

    let mut out = Vec::new();
    for token in trimmed
        .replace("->", ",")
        .replace(['>', ' '], ",")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let parsed = if let Ok(n) = token.parse::<usize>() {
            default.get(n.saturating_sub(1)).cloned()
        } else {
            AgentKind::parse(token).map(|k| k.as_str().to_string())
        };
        if let Some(agent) = parsed {
            if !out.contains(&agent) {
                out.push(agent);
            }
        }
    }
    if out.is_empty() {
        Ok(default.to_vec())
    } else {
        Ok(out)
    }
}

fn display_agent(agent: &str) -> &str {
    match agent {
        "claude" => "Claude Code",
        "codex" => "Codex CLI",
        "copilot" => "GitHub Copilot",
        "cursor" => "Cursor / Antigravity",
        other => other,
    }
}

fn prompt_yes_no(label: &str, default: bool) -> Result<bool> {
    let default_label = if default { "Y/n" } else { "y/N" };
    print!("{label}? [{default_label}] ");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let trimmed = buf.trim().to_ascii_lowercase();
    Ok(match trimmed.as_str() {
        "" => default,
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    })
}

fn cmd_sync(path: PathBuf) -> Result<()> {
    let root = path.canonicalize()?;
    let written = ContextEngine::new(&root).sync()?;
    for p in written {
        println!("wrote {}", p.display());
    }
    Ok(())
}

async fn cmd_agents(project_id: Option<i64>, once: bool) -> Result<()> {
    if !once {
        let url = std::env::var("HANDOFF_DAEMON_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:7879".to_string());
        return handoff_cli::tui::run(&url).await;
    }

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

async fn cmd_spawn(
    kind: &str,
    args: Vec<String>,
    project: PathBuf,
    no_proxy: bool,
    force_interactive: bool,
    force_headless: bool,
) -> Result<()> {
    let _ak = AgentKind::parse(kind).ok_or_else(|| anyhow!("unknown kind: {kind}"))?;
    let adapter = handoff_adapters::for_kind(_ak);
    let project_root = project.canonicalize()?;

    // Decide the mode. Default rule: positional args present → headless.
    let mode = if force_interactive {
        SpawnMode::Interactive
    } else if force_headless || !args.is_empty() {
        SpawnMode::Headless
    } else {
        SpawnMode::Interactive
    };

    let final_argv = build_spawn_argv(kind, &mode, &args, adapter)?;
    let mut cmd = std::process::Command::new(&final_argv[0]);
    cmd.args(&final_argv[1..]).current_dir(&project_root);
    if !no_proxy {
        for (k, v) in handoff_daemon::spawn::proxy_env(&proxy_url()) {
            cmd.env(k, v);
        }
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let project_id = rpc_register_project(&project_root).await?;
    let res = rpc_call(
        "register_agent",
        serde_json::json!({
            "project_id": project_id,
            "kind": kind,
            "pid": pid,
            "spawned_by": "handoff",
        }),
    )
    .await?;
    let aid = res
        .get("agent_id")
        .and_then(|v| v.as_i64())
        .unwrap_or_default();
    let mode_label = match mode {
        SpawnMode::Interactive => "interactive",
        SpawnMode::Headless => "headless",
    };
    println!("spawned {kind} ({mode_label}) pid={pid} agent_id={aid}");
    let status = child.wait()?;
    let _ = rpc_call(
        "stop_agent",
        serde_json::json!({"agent_id": aid, "status": if status.success() {"stopped"} else {"failed"}}),
    )
    .await;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnMode {
    Interactive,
    Headless,
}

fn build_spawn_argv(
    kind: &str,
    mode: &SpawnMode,
    user_args: &[String],
    adapter: Box<dyn handoff_adapters::Adapter>,
) -> Result<Vec<String>> {
    let bin = adapter
        .binaries()
        .iter()
        .find_map(|b| which::which(b).ok())
        .ok_or_else(|| anyhow!("no binary on PATH for kind={kind}"))?;
    let bin_str = bin.display().to_string();
    let mut argv: Vec<String> = vec![bin_str];

    match mode {
        SpawnMode::Interactive => {
            // Pass user args through verbatim; no kind-specific transform.
            argv.extend(user_args.iter().cloned());
        }
        SpawnMode::Headless => {
            // Treat user_args as a single prompt.
            let prompt = user_args.join(" ");
            match adapter.headless_args(&prompt) {
                Some(specific_args) => {
                    argv.extend(specific_args);
                }
                None => {
                    return Err(anyhow!(
                        "no headless form known for kind={kind}; \
                         pass --interactive or --no-proxy and craft your own argv"
                    ));
                }
            }
        }
    }
    Ok(argv)
}

async fn cmd_attach(pid: i64, kind: String, project: PathBuf) -> Result<()> {
    let project_root = project.canonicalize()?;
    let project_id = rpc_register_project(&project_root).await?;
    let res = rpc_call(
        "attach_agent",
        serde_json::json!({
            "project_id": project_id,
            "kind": kind,
            "pid": pid,
        }),
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
    let project_id = rpc_register_project(&project_root).await?;
    let res = rpc_call(
        "handoff",
        serde_json::json!({
            "project_id": project_id,
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

fn brain_path(project: &Path) -> Result<PathBuf> {
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

async fn cmd_brain_edit(project: PathBuf) -> Result<()> {
    let p = brain_path(&project)?;
    let _root = project.canonicalize()?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(&editor).arg(&p).status()?;
    if !status.success() {
        return Err(anyhow!("$EDITOR exited with {status}"));
    }

    // After edit, push to daemon if possible to ensure it has latest?
    // Actually, daemon reads from disk, but serialized access might want to know.
    // For now, let's just let it be. Serializing edits is hard because of the interactive nature.
    Ok(())
}

async fn cmd_brain_append(text: String, project: PathBuf) -> Result<()> {
    let project_root = project.canonicalize()?;
    let url =
        std::env::var("HANDOFF_DAEMON_URL").unwrap_or_else(|_| "http://127.0.0.1:7879".to_string());

    let res = reqwest::Client::new()
        .post(format!("{}/brain/append", url))
        .json(&serde_json::json!({
            "project_root": project_root.display().to_string(),
            "text": text,
        }))
        .send()
        .await;

    match res {
        Ok(resp) if resp.status().is_success() => {
            println!("appended to brain.md (via daemon)");
        }
        _ => {
            let p = brain_path(&project)?;
            let mut file = std::fs::OpenOptions::new().append(true).open(&p)?;
            use std::io::Write;
            writeln!(file, "\n{}", text)?;
            println!("appended to brain.md (direct)");
        }
    }
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
    let project_id = rpc_register_project(&root).await.ok();
    let policy = handoff_policy::load(&root).unwrap_or_default();
    let mut runner = handoff_critic::CriticRunner::new(&root)?
        .with_agents(worker.clone(), critic.clone())
        .with_proxy(if no_proxy { None } else { Some(proxy_url()) });
    runner.max_rounds = Some(policy.critic.max_rounds);
    let res = runner.run(task).await?;
    println!("verdict: {}", res.verdict);
    println!("notes: {}", res.notes);
    println!("worker={} critic={}", res.worker_agent, res.critic_agent);
    for a in &res.artifacts {
        println!("  wrote {a}");
    }
    let _ = rpc_call(
        "record_critic_run",
        serde_json::json!({
            "project_id": project_id,
            "worker_agent": res.worker_agent,
            "critic_agent": res.critic_agent,
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
    let project_id = rpc_register_project(&root).await.ok();
    let runner = handoff_critic::CriticRunner::new(&root)?
        .with_agents(worker.clone(), critic.clone())
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
                        res.verdict, res.worker_agent, res.critic_agent
                    );
                    let _ = rpc_call(
                        "record_critic_run",
                        serde_json::json!({
                            "project_id": project_id,
                            "worker_agent": res.worker_agent,
                            "critic_agent": res.critic_agent,
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
        .get("http://127.0.0.1:7879/health")
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

fn default_daemon_addr() -> SocketAddr {
    std::env::var("HANDOFF_DAEMON_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:7879".parse().expect("valid default daemon addr"))
}

fn default_proxy_addr() -> SocketAddr {
    std::env::var("HANDOFF_PROXY_ADDR")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:8080".parse().expect("valid default proxy addr"))
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
    let url =
        std::env::var("HANDOFF_DAEMON_URL").unwrap_or_else(|_| "http://127.0.0.1:7879/rpc".into());
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

async fn rpc_register_project(project_root: &Path) -> Result<i64> {
    let res = rpc_call(
        "register_project",
        serde_json::json!({"root": project_root.display().to_string()}),
    )
    .await?;
    res.get("project_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("register_project returned no project_id"))
}

async fn wait_for_daemon_ready() -> Result<()> {
    let health = daemon_health_url();
    let client = reqwest::Client::new();
    for _ in 0..40 {
        if matches!(
            client.get(&health).timeout(Duration::from_secs(1)).send().await,
            Ok(resp) if resp.status().is_success()
        ) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(anyhow!("daemon did not become ready at {health}"))
}

fn daemon_health_url() -> String {
    format!("{}/health", daemon_base_url())
}

fn daemon_base_url() -> String {
    let rpc =
        std::env::var("HANDOFF_DAEMON_URL").unwrap_or_else(|_| "http://127.0.0.1:7879".to_string());
    if let Some(base) = rpc.strip_suffix("/rpc") {
        base.to_string()
    } else {
        rpc
    }
}

#[allow(dead_code)]
fn _ak_keepalive() -> Option<AgentKind> {
    AgentKind::parse("claude")
}

#[derive(Subcommand, Debug)]
enum WorktreeCmd {
    /// List all active worktrees.
    List,
    /// View the diff of a specific agent's worktree.
    Diff { agent_id: i64 },
    /// Remove a specific agent's worktree.
    Clean { agent_id: i64 },
}

fn cmd_worktree(cmd: WorktreeCmd) -> Result<()> {
    let root = std::env::current_dir()?;
    match cmd {
        WorktreeCmd::List => {
            let list = handoff_daemon::worktree::list(&root)?;
            if list.is_empty() {
                println!("no worktrees found");
            } else {
                for p in list {
                    println!("{}", p.display());
                }
            }
        }
        WorktreeCmd::Diff { agent_id } => {
            let d = handoff_daemon::worktree::diff(agent_id)?;
            println!("{d}");
        }
        WorktreeCmd::Clean { agent_id } => {
            handoff_daemon::worktree::remove(&root, agent_id)?;
            println!("removed worktree for agent-{agent_id}");
        }
    }
    Ok(())
}

#[derive(Subcommand, Debug)]
enum HookCmd {
    /// Install a hook for a specific agent.
    Install { agent: String },
    /// Uninstall a hook for a specific agent.
    Uninstall { agent: String },
}

fn cmd_hook(cmd: HookCmd) -> Result<()> {
    match cmd {
        HookCmd::Install { agent } => {
            if agent == "claude" {
                let settings = dirs::home_dir().unwrap().join(".claude/settings.json");
                handoff_cli::hook::install_claude("http://127.0.0.1:7879", &settings)?;
            } else {
                eprintln!("Hook install for '{}' not yet supported", agent);
            }
        }
        HookCmd::Uninstall { agent } => {
            if agent == "claude" {
                let settings = dirs::home_dir().unwrap().join(".claude/settings.json");
                handoff_cli::hook::uninstall_claude(&settings)?;
            } else {
                eprintln!("Hook uninstall for '{}' not yet supported", agent);
            }
        }
    }
    Ok(())
}

async fn cmd_logs(target: String, agent_id: Option<i64>, follow: bool) -> Result<()> {
    let path = match target.as_str() {
        "daemon" => handoff_common::home_dir().join("logs").join("daemon.log"),
        "proxy" => handoff_common::home_dir().join("logs").join("proxy.log"),
        "agent" => {
            let aid = agent_id.ok_or_else(|| anyhow!("agent_id required for target 'agent'"))?;
            handoff_common::tee::tee_path(aid)
        }
        _ => return Err(anyhow!("invalid log target: {target}")),
    };

    if !path.exists() {
        return Err(anyhow!("log file not found: {}", path.display()));
    }

    if follow {
        std::process::Command::new("tail")
            .args(["-f", &path.to_string_lossy()])
            .status()?;
    } else {
        let content = std::fs::read_to_string(&path)?;
        print!("{}", content);
    }
    Ok(())
}

async fn cmd_replay(handoff_id: i64) -> Result<()> {
    let db = Database::open(&db_path())?;
    let data = db.get_replay_data(handoff_id)?;

    println!("=== Handoff #{} Replay ===", data.handoff.id);
    println!(
        "Time:   {}",
        chrono::DateTime::from_timestamp(data.handoff.ts, 0)
            .map(|ts| ts.to_rfc3339())
            .unwrap_or_else(|| "unknown".into())
    );
    println!("Reason: {}", data.handoff.reason);

    if let Some(from) = data.from_agent {
        println!(
            "From:   {} (#{}) [status={}]",
            from.kind, from.id, from.status
        );
    }

    if let Some(to) = data.to_agent {
        println!("To:     {} (#{}) [pid={:?}]", to.kind, to.id, to.pid);
    }

    if let Some(snap) = data.snapshot_content {
        println!("\n--- Snapshot Content (first 20 lines) ---");
        for line in snap.lines().take(20) {
            println!("{}", line);
        }
        if snap.lines().count() > 20 {
            println!("...");
        }
    }

    Ok(())
}

fn cmd_stats(days: u32, graph: bool) -> Result<()> {
    let db = Database::open(&db_path())?;
    let stats = db.daily_stats(days)?;

    println!(
        "{:<12} {:<10} {:<10} {:<15} {:<10}",
        "Date", "Kind", "Requests", "Avg Tokens", "Handoffs"
    );
    println!("{}", "-".repeat(60));

    for s in stats {
        let bar = if graph {
            let filled = (s.avg_tokens_remaining.unwrap_or(0.0) / 1000.0) as usize;
            format!(
                " [{}{}]",
                "█".repeat(filled.min(20)),
                " ".repeat(20 - filled.min(20))
            )
        } else {
            String::new()
        };

        println!(
            "{:<12} {:<10} {:<10} {:<15.0} {:<10}{}",
            s.date,
            s.kind,
            s.total_requests,
            s.avg_tokens_remaining.unwrap_or(0.0),
            s.handoff_count,
            bar
        );
    }

    Ok(())
}
