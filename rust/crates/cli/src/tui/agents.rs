use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Row, Table},
    Frame,
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Deserialize, Default, Clone)]
pub struct AgentSummary {
    pub id: i64,
    pub kind: String,
    pub pid: Option<i32>,
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
    pub raw_count: usize,
}

impl DashboardData {
    fn observed_requests(&self) -> i64 {
        self.agents.iter().map(|a| a.total_requests).sum()
    }

    fn observed_429s(&self) -> i64 {
        self.agents.iter().map(|a| a.rate_limited_count).sum()
    }

    fn sampled_agents(&self) -> usize {
        self.agents
            .iter()
            .filter(|a| a.last_sample_ts.is_some())
            .count()
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
    Ok(prepare_dashboard_data(&mut agents))
}

fn prepare_dashboard_data(raw_agents: &mut [AgentSummary]) -> DashboardData {
    let procs = handoff_adapters::snapshot_procs();
    let live_pids: HashSet<i64> = procs.iter().map(|p| p.pid).collect();
    let proc_cmds: HashMap<i64, String> = procs
        .iter()
        .map(|p| (p.pid, p.cmdline.join(" ").to_ascii_lowercase()))
        .collect();

    let raw_count = raw_agents.len();
    let mut stale_count = 0;
    let mut hidden_internal_count = 0;
    let mut agents = Vec::new();

    for agent in raw_agents.iter_mut() {
        let pid = agent.pid.map(i64::from);
        agent.pid_alive = pid.map(|p| live_pids.contains(&p)).unwrap_or(false);
        agent.internal_process = pid
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

        agents.push(agent.clone());
    }

    DashboardData {
        agents,
        stale_count,
        hidden_internal_count,
        raw_count,
    }
}

fn is_internal_handoff_cmd(cmd: &str) -> bool {
    cmd.contains("handoff") && (cmd.contains(" daemon ") || cmd.contains(" proxy "))
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
                    format!("#{}", a.id),
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
            Constraint::Length(10),
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
         live agents: {} | stale records hidden: {} | services hidden: {} | tracked records: {}\n\
         requests seen: {} | agents with rate samples: {} | 429s: {}\n\
         rate limits appear after Claude/Codex traffic goes through the Handsoff proxy",
        data.agents.len(),
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

fn budget_label(agent: &AgentSummary) -> String {
    match agent.tokens_remaining {
        Some(tokens) => match agent.tokens_reset_at {
            Some(reset) => format!("{tokens} reset {}", format_epoch_delta(reset)),
            None => tokens.to_string(),
        },
        None => "waiting".to_string(),
    }
}

fn request_budget_label(agent: &AgentSummary) -> String {
    agent
        .requests_remaining
        .map(|r| r.to_string())
        .unwrap_or_else(|| "waiting".to_string())
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
    fn budget_label_formats_future_reset() {
        let agent = AgentSummary {
            tokens_remaining: Some(1200),
            tokens_reset_at: Some(chrono::Utc::now().timestamp() + 120),
            ..Default::default()
        };
        assert!(budget_label(&agent).contains("reset in"));
    }
}
