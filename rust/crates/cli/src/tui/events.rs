pub async fn fetch_handoffs(daemon_url: &str) -> anyhow::Result<Vec<String>> {
    let resp = reqwest::Client::new()
        .get(format!("{}/handoffs", daemon_url))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let mut out = Vec::new();
    if let Some(arr) = resp["handoffs"].as_array() {
        for row in arr {
            let reason = row["reason"].as_str().unwrap_or("?");
            let from = row["from_agent_id"].as_i64().unwrap_or(0);
            let to = row["to_agent_id"].as_i64().unwrap_or(0);
            out.push(format!("Handoff: {} -> {} ({})", from, to, reason));
        }
    }
    Ok(out)
}
