use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem},
    Frame,
};
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
pub struct HandoffSummary {
    pub id: i64,
    pub from_agent_id: Option<i64>,
    pub to_agent_id: Option<i64>,
    pub reason: String,
    pub ts: i64,
}

pub async fn fetch(daemon_url: &str) -> anyhow::Result<Vec<HandoffSummary>> {
    let resp = reqwest::Client::new()
        .get(format!("{}/handoffs", daemon_url))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let handoffs = serde_json::from_value(resp["handoffs"].clone()).unwrap_or_default();
    Ok(handoffs)
}

pub fn render(frame: &mut Frame, handoffs: &[HandoffSummary]) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title
            Constraint::Min(10),   // timeline
            Constraint::Length(3), // footer
        ])
        .split(frame.area());

    // Title
    let title = Block::default()
        .title(" handoff timeline ")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(title, chunks[0]);

    // Timeline List
    let items: Vec<ListItem> = handoffs
        .iter()
        .map(|h| {
            let time = chrono::DateTime::from_timestamp(h.ts, 0)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_else(|| "??:??:??".into());

            let from_str = h
                .from_agent_id
                .map(|id| format!("#{id}"))
                .unwrap_or_else(|| "Unknown".to_string());
            let to_str = h
                .to_agent_id
                .map(|id| format!("#{id}"))
                .unwrap_or_else(|| "Unknown".to_string());

            let content = format!(
                "[{}] Agent {} → {} | Reason: {}",
                time, from_str, to_str, h.reason
            );
            ListItem::new(content)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title("Handoff History")
                .borders(Borders::ALL),
        )
        .style(Style::default().fg(Color::White));

    frame.render_widget(list, chunks[1]);

    // Footer
    let footer = Block::default()
        .title(" q: quit | tab: switch view ")
        .borders(Borders::TOP);
    frame.render_widget(footer, chunks[2]);
}
