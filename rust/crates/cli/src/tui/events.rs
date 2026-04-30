pub async fn fetch_handoffs(daemon_url: &str) -> anyhow::Result<Vec<String>> {
    let resp = reqwest::Client::new()
        .get(format!("{}/events", daemon_url))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let mut out = Vec::new();
    if let Some(arr) = resp["events"].as_array() {
        for row in arr {
            match row["kind"].as_str().unwrap_or_default() {
                "handoff" => {
                    let reason = row["reason"].as_str().unwrap_or("?");
                    let from_str = row["from_agent_id"]
                        .as_i64()
                        .map(|id| format!("#{id}"))
                        .unwrap_or_else(|| "Unknown".to_string());
                    let to_str = row["to_agent_id"]
                        .as_i64()
                        .map(|id| format!("#{id}"))
                        .unwrap_or_else(|| "Unknown".to_string());
                    out.push(format!("handoff: agent {from_str} -> {to_str} ({reason})"));
                }
                "agent_spawn" => {
                    let agent = row["agent_kind"].as_str().unwrap_or("agent");
                    let id = row["id"].as_i64().unwrap_or(0);
                    let pid = row["pid"].as_i64().unwrap_or(0);
                    out.push(format!("agent: {agent} registered as #{id} pid={pid}"));
                }
                _ => {}
            }
        }
    }
    if out.is_empty() {
        out.push("no activity yet".into());
    }
    Ok(out)
}
