use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Row, Table},
    Frame,
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use handoff_adapters::{all as all_adapters, ProcInfo};
use handoff_common::{daemon_pidfile, proxy_pidfile};

#[derive(Default, Clone)]
struct KindUsage {
    tokens_remaining: Option<i64>,
    requests_remaining: Option<i64>,
    tokens_reset_at: Option<i64>,
    last_sample_ts: Option<i64>,
    raw_headers: Option<serde_json::Value>,
    total_requests: i64,
    rate_limited_count: i64,
    last_429_at: Option<i64>,
}

#[derive(Deserialize, Default, Clone)]
pub struct AgentSummary {
    pub id: i64,
    pub kind: String,
    pub pid: Option<i64>,
    pub status: String,
    #[serde(default)]
    pub spawned_by: String,
    #[serde(default)]
    pub started_at: i64,
    pub tokens_remaining: Option<i64>,
    pub requests_remaining: Option<i64>,
    #[serde(default)]
    pub tokens_reset_at: Option<i64>,
    #[serde(default)]
    pub last_sample_ts: Option<i64>,
    #[serde(default)]
    pub raw_headers: Option<serde_json::Value>,
    #[serde(default)]
    pub total_requests: i64,
    #[serde(default)]
    pub rate_limited_count: i64,
    #[serde(default)]
    pub last_429_at: Option<i64>,
    #[serde(skip)]
    pub pid_alive: bool,
    #[serde(skip)]
    pub internal_process: bool,
}

#[derive(Default, Clone)]
pub struct DashboardData {
    pub agents: Vec<AgentSummary>,
    pub stale_count: usize,
    pub hidden_internal_count: usize,
    pub discovered_count: usize,
    pub raw_count: usize,
    pub tracked_requests_seen: i64,
    pub tracked_429s_seen: i64,
    pub tracked_sampled_agents: usize,
}

impl DashboardData {
    fn observed_requests(&self) -> i64 {
        self.tracked_requests_seen
    }

    fn observed_429s(&self) -> i64 {
        self.tracked_429s_seen
    }

    fn sampled_agents(&self) -> usize {
        self.tracked_sampled_agents
    }
}

pub async fn fetch(daemon_url: &str) -> anyhow::Result<DashboardData> {
    let resp = reqwest::Client::new()
        .post(format!("{}/rpc", daemon_url))
        .json(&serde_json::json!({"method": "list_agents", "params": {}}))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let mut agents: Vec<AgentSummary> =
        serde_json::from_value(resp["result"]["agents"].clone()).unwrap_or_default();
    let procs = handoff_adapters::snapshot_procs();
    let service_pids = read_service_pids();
    Ok(prepare_dashboard_data(&mut agents, &procs, &service_pids))
}

fn prepare_dashboard_data(
    raw_agents: &mut [AgentSummary],
    procs: &[ProcInfo],
    service_pids: &HashSet<i64>,
) -> DashboardData {
    let live_pids: HashSet<i64> = procs.iter().map(|p| p.pid).collect();
    let proc_cmds: HashMap<i64, String> = procs
        .iter()
        .map(|p| {
            (
                p.pid,
                format!("{} {}", p.name, p.cmdline.join(" ")).to_ascii_lowercase(),
            )
        })
        .collect();

    let raw_count = raw_agents.len();
    let tracked_requests_seen = raw_agents.iter().map(|a| a.total_requests).sum();
    let tracked_429s_seen = raw_agents.iter().map(|a| a.rate_limited_count).sum();
    let tracked_sampled_agents = raw_agents
        .iter()
        .filter(|a| a.last_sample_ts.is_some())
        .count();
    let usage_by_kind = latest_usage_by_kind(raw_agents);
    let mut stale_count = 0;
    let mut hidden_internal_count = 0;
    let mut discovered_count = 0;
    let mut agents = Vec::new();
    let mut visible_pids = HashSet::new();

    for agent in raw_agents.iter_mut() {
        let pid = agent.pid;
        agent.pid_alive = pid.map(|p| live_pids.contains(&p)).unwrap_or(false);
        agent.internal_process = pid.map(|p| service_pids.contains(&p)).unwrap_or(false)
            || pid
                .and_then(|p| proc_cmds.get(&p))
                .map(|cmd| is_internal_handoff_cmd(cmd))
                .unwrap_or(false);

        if agent.internal_process {
            hidden_internal_count += 1;
            continue;
        }

        if !agent.pid_alive {
            stale_count += 1;
            continue;
        }

        if let Some(pid) = pid {
            visible_pids.insert(pid);
        }
        agents.push(agent.clone());
    }

    for adapter in all_adapters() {
        for detected in adapter.detect(procs) {
            let detected_cmd = format!("{} {}", detected.name, detected.cmdline.join(" "));
            if service_pids.contains(&detected.pid)
                || visible_pids.contains(&detected.pid)
                || is_internal_handoff_cmd(&detected_cmd.to_ascii_lowercase())
            {
                continue;
            }

            visible_pids.insert(detected.pid);
            discovered_count += 1;
            let mut agent = AgentSummary {
                id: 0,
                kind: adapter.kind().as_str().to_string(),
                pid: Some(detected.pid),
                status: "detected".into(),
                spawned_by: "process".into(),
                pid_alive: true,
                ..Default::default()
            };
            if let Some(usage) = usage_by_kind.get(&agent.kind) {
                apply_kind_usage(&mut agent, usage);
            }
            agents.push(agent);
        }
    }

    DashboardData {
        agents,
        stale_count,
        hidden_internal_count,
        discovered_count,
        raw_count,
        tracked_requests_seen,
        tracked_429s_seen,
        tracked_sampled_agents,
    }
}

fn latest_usage_by_kind(agents: &[AgentSummary]) -> HashMap<String, KindUsage> {
    let mut usage = HashMap::<String, KindUsage>::new();
    for agent in agents {
        let entry = usage.entry(agent.kind.clone()).or_default();
        entry.total_requests += agent.total_requests;
        entry.rate_limited_count += agent.rate_limited_count;
        entry.last_429_at = latest_ts(entry.last_429_at, agent.last_429_at);

        let is_newer_sample = match (agent.last_sample_ts, entry.last_sample_ts) {
            (Some(a), Some(b)) => a > b,
            (Some(_), None) => true,
            _ => false,
        };
        if is_newer_sample {
            entry.tokens_remaining = agent.tokens_remaining;
            entry.requests_remaining = agent.requests_remaining;
            entry.tokens_reset_at = agent.tokens_reset_at;
            entry.last_sample_ts = agent.last_sample_ts;
            entry.raw_headers = agent.raw_headers.clone();
        }
    }
    usage
}

fn apply_kind_usage(agent: &mut AgentSummary, usage: &KindUsage) {
    agent.tokens_remaining = usage.tokens_remaining;
    agent.requests_remaining = usage.requests_remaining;
    agent.tokens_reset_at = usage.tokens_reset_at;
    agent.last_sample_ts = usage.last_sample_ts;
    agent.raw_headers = usage.raw_headers.clone();
    agent.total_requests = usage.total_requests;
    agent.rate_limited_count = usage.rate_limited_count;
    agent.last_429_at = usage.last_429_at;
}

fn latest_ts(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn is_internal_handoff_cmd(cmd: &str) -> bool {
    cmd.contains("handoff") && (cmd.contains(" daemon ") || cmd.contains(" proxy "))
}

fn read_service_pids() -> HashSet<i64> {
    [daemon_pidfile(), proxy_pidfile()]
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|s| s.trim().parse::<i64>().ok())
        .collect()
}

pub fn render(frame: &mut Frame, data: &DashboardData, handoffs: &[String]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // summary
            Constraint::Min(6),    // agents table
            Constraint::Length(7), // handoffs panel
            Constraint::Length(3), // footer
        ])
        .split(frame.area());

    let summary = render_summary(data);
    let summary_block = Block::default()
        .title(" handoff live dashboard ")
        .borders(Borders::ALL);
    frame.render_widget(Paragraph::new(summary).block(summary_block), chunks[0]);

    let rows: Vec<Row> = if data.agents.is_empty() {
        vec![Row::new(vec![
            "no live agents yet".to_string(),
            "".to_string(),
            "".to_string(),
            "waiting".to_string(),
            "start claude, codex, or gh copilot normally".to_string(),
            "".to_string(),
            "".to_string(),
            "".to_string(),
            "".to_string(),
        ])]
    } else {
        data.agents
            .iter()
            .map(|a| {
                Row::new(vec![
                    a.kind.clone(),
                    id_label(a),
                    a.pid.map(|p| p.to_string()).unwrap_or_default(),
                    state_label(a),
                    budget_label(a),
                    request_budget_label(a),
                    a.total_requests.to_string(),
                    a.rate_limited_count.to_string(),
                    last_seen_label(a),
                ])
            })
            .collect()
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(16),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(vec![
            "Agent",
            "ID",
            "PID",
            "State",
            "Token budget",
            "Req budget",
            "Seen",
            "429",
            "Last",
        ])
        .style(Style::default().fg(Color::Yellow)),
    )
    .block(
        Block::default()
            .title("Running Agents")
            .borders(Borders::ALL),
    );

    frame.render_widget(table, chunks[1]);

    let handoff_text = handoffs.join("\n");
    let handoff_block = Block::default()
        .title("Recent Activity")
        .borders(Borders::ALL);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(handoff_text).block(handoff_block),
        chunks[2],
    );

    let footer = Block::default()
        .title(" q: quit | tab: timeline | auto-refresh: 500ms ")
        .borders(Borders::TOP);
    frame.render_widget(footer, chunks[3]);
}

fn render_summary(data: &DashboardData) -> String {
    let observer = if data.sampled_agents() > 0 || data.observed_requests() > 0 {
        "observing provider traffic"
    } else {
        "waiting for first proxied provider response"
    };

    format!(
        "daemon connected | observer: {observer}\n\
         live agents: {} | discovered now: {} | stale records hidden: {} | services hidden: {} | tracked records: {}\n\
         requests seen: {} | agents with rate samples: {} | 429s: {}\n\
         rate limits appear after provider traffic goes through the proxy or companion reports usage",
        data.agents.len(),
        data.discovered_count,
        data.stale_count,
        data.hidden_internal_count,
        data.raw_count,
        data.observed_requests(),
        data.sampled_agents(),
        data.observed_429s(),
    )
}

fn state_label(agent: &AgentSummary) -> String {
    if agent.pid_alive {
        agent.status.clone()
    } else {
        "stale".to_string()
    }
}

fn id_label(agent: &AgentSummary) -> String {
    if agent.id > 0 {
        format!("#{}", agent.id)
    } else {
        "live".to_string()
    }
}

fn budget_label(agent: &AgentSummary) -> String {
    match agent.tokens_remaining {
        Some(tokens) => match agent.tokens_reset_at {
            Some(reset) => format!("{tokens} reset {}", format_epoch_delta(reset)),
            None => tokens.to_string(),
        },
        None => anthropic_remaining_label(agent, "7d")
            .or_else(|| anthropic_remaining_label(agent, "unified"))
            .unwrap_or_else(|| "waiting".to_string()),
    }
}

fn request_budget_label(agent: &AgentSummary) -> String {
    agent
        .requests_remaining
        .map(|r| r.to_string())
        .or_else(|| anthropic_remaining_label(agent, "5h"))
        .unwrap_or_else(|| "waiting".to_string())
}

fn anthropic_remaining_label(agent: &AgentSummary, window: &str) -> Option<String> {
    let raw = agent.raw_headers.as_ref()?.as_object()?;
    let key = match window {
        "5h" => "anthropic-ratelimit-unified-5h-utilization",
        "7d" => "anthropic-ratelimit-unified-7d-utilization",
        "unified" => "anthropic-ratelimit-unified-utilization",
        _ => return None,
    };
    let utilization = raw.get(key)?.as_str()?.parse::<f64>().ok()?;
    let remaining = ((1.0 - utilization).clamp(0.0, 1.0) * 100.0).round() as i64;
    Some(format!("{remaining}% left ({window})"))
}

fn last_seen_label(agent: &AgentSummary) -> String {
    let ts = agent.last_sample_ts.or(agent.last_429_at);
    ts.map(format_epoch_age)
        .unwrap_or_else(|| "no sample".to_string())
}

fn format_epoch_age(ts: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let delta = now.saturating_sub(ts).max(0);
    format_duration(delta, "ago")
}

fn format_epoch_delta(ts: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    if ts >= now {
        format!("in {}", format_duration_value(ts - now))
    } else {
        format_epoch_age(ts)
    }
}

fn format_duration(delta: i64, suffix: &str) -> String {
    format!("{} {suffix}", format_duration_value(delta))
}

fn format_duration_value(delta: i64) -> String {
    if delta < 60 {
        format!("{delta}s")
    } else if delta < 3600 {
        format!("{}m", delta / 60)
    } else if delta < 86_400 {
        format!("{}h", delta / 3600)
    } else {
        format!("{}d", delta / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i64, name: &str, cmdline: &[&str]) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            cmdline: cmdline.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn internal_handoff_processes_are_detected() {
        assert!(is_internal_handoff_cmd(
            "/tmp/handoff target/debug/handoff proxy start"
        ));
        assert!(is_internal_handoff_cmd("/usr/local/bin/handoff daemon run"));
        assert!(!is_internal_handoff_cmd("/usr/local/bin/claude"));
    }

    #[test]
    fn summary_explains_waiting_state() {
        let summary = render_summary(&DashboardData::default());
        assert!(summary.contains("waiting for first proxied provider response"));
        assert!(summary.contains("live agents: 0"));
    }

    #[test]
    fn dashboard_merges_live_discovered_agents() {
        let procs = vec![proc(10, "claude", &["/usr/local/bin/claude"])];
        let mut agents = Vec::new();
        let data = prepare_dashboard_data(&mut agents, &procs, &HashSet::new());
        assert_eq!(data.agents.len(), 1);
        assert_eq!(data.agents[0].kind, "claude");
        assert_eq!(data.agents[0].status, "detected");
        assert_eq!(data.discovered_count, 1);
    }

    #[test]
    fn dashboard_applies_kind_usage_to_discovered_agents() {
        let procs = vec![proc(10, "claude", &["/usr/local/bin/claude"])];
        let mut agents = vec![AgentSummary {
            id: 2,
            kind: "claude".into(),
            pid: Some(99),
            status: "running".into(),
            tokens_remaining: Some(1200),
            requests_remaining: Some(44),
            last_sample_ts: Some(100),
            total_requests: 3,
            ..Default::default()
        }];

        let data = prepare_dashboard_data(&mut agents, &procs, &HashSet::new());
        assert_eq!(data.agents.len(), 1);
        assert_eq!(data.agents[0].pid, Some(10));
        assert_eq!(data.agents[0].tokens_remaining, Some(1200));
        assert_eq!(data.agents[0].requests_remaining, Some(44));
        assert_eq!(data.agents[0].last_sample_ts, Some(100));
        assert_eq!(data.agents[0].total_requests, 3);
    }

    #[test]
    fn dashboard_formats_anthropic_unified_headers() {
        let raw_headers = serde_json::json!({
            "anthropic-ratelimit-unified-7d-utilization": "0.99",
            "anthropic-ratelimit-unified-5h-utilization": "0.06"
        });
        let agent = AgentSummary {
            raw_headers: Some(raw_headers),
            ..Default::default()
        };

        assert_eq!(budget_label(&agent), "1% left (7d)");
        assert_eq!(request_budget_label(&agent), "94% left (5h)");
    }

    #[test]
    fn dashboard_hides_service_pid_even_when_record_kind_is_agent() {
        let procs = vec![proc(99, "handoff", &["/usr/local/bin/handoff", "proxy"])];
        let mut service_pids = HashSet::new();
        service_pids.insert(99);
        let mut agents = vec![AgentSummary {
            id: 2,
            kind: "claude".into(),
            pid: Some(99),
            status: "running".into(),
            total_requests: 44,
            ..Default::default()
        }];

        let data = prepare_dashboard_data(&mut agents, &procs, &service_pids);
        assert!(data.agents.is_empty());
        assert_eq!(data.hidden_internal_count, 1);
        assert_eq!(data.observed_requests(), 44);
    }

    #[test]
    fn budget_label_formats_future_reset() {
        let agent = AgentSummary {
            tokens_remaining: Some(1200),
            tokens_reset_at: Some(chrono::Utc::now().timestamp() + 120),
            ..Default::default()
        };
        assert!(budget_label(&agent).contains("reset in"));
    }
}
