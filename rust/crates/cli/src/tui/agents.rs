use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Row, Table},
    Frame,
};
use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
pub struct AgentSummary {
    pub id: i64,
    pub kind: String,
    pub pid: Option<i32>,
    pub status: String,
    pub tokens_remaining: Option<i64>,
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

    let agents = serde_json::from_value(resp["result"]["agents"].clone()).unwrap_or_default();
    Ok(agents)
}

pub fn render(frame: &mut Frame, agents: &[AgentSummary], handoffs: &[String]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title
            Constraint::Min(6),    // agents table
            Constraint::Length(7), // handoffs panel
            Constraint::Length(3), // footer
        ])
        .split(frame.area());

    // Title bar
    let title = Block::default()
        .title(" handoff agents ")
        .borders(Borders::ALL);
    frame.render_widget(title, chunks[0]);

    // Agents table
    let rows: Vec<Row> = agents
        .iter()
        .map(|a| {
            let tokens = a
                .tokens_remaining
                .map(|r| r.to_string())
                .unwrap_or("—".to_string());
            Row::new(vec![
                a.kind.clone(),
                a.pid.map(|p| p.to_string()).unwrap_or_default(),
                a.status.clone(),
                tokens,
                a.requests_remaining
                    .map(|r| r.to_string())
                    .unwrap_or("—".to_string()),
            ])
        })
        .collect();

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
    .header(
        Row::new(vec!["Kind", "PID", "Status", "Tokens", "Requests"])
            .style(Style::default().fg(Color::Yellow)),
    )
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
