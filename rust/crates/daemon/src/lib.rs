//! HTTP daemon: /ingest for the proxy, /rpc for the CLI.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use handoff_adapters::all as all_adapters;
use handoff_common::RateSample;
use handoff_storage::Database;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;
use tracing::warn;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
}

#[derive(Debug, Deserialize)]
pub struct IngestPayload {
    pub kind: String,
    pub host: String,
    pub status_code: u16,
    pub pid: Option<i64>,
    pub sample: Option<RateSample>,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub ok: bool,
    pub agent_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ingest", post(ingest))
        .route("/rpc", post(rpc))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({"ok": true, "ts": Utc::now().timestamp()}))
}

async fn ingest(
    State(s): State<AppState>,
    Json(payload): Json<IngestPayload>,
) -> impl IntoResponse {
    let agent_id = match resolve_agent_id(&s, &payload) {
        Ok(id) => id,
        Err(e) => {
            warn!("ingest resolve failed: {e}");
            None
        }
    };
    if let Some(aid) = agent_id {
        let _ = s.db.bump_request_count(aid, payload.status_code);
        if let Some(sample) = &payload.sample {
            let _ = s.db.insert_rate_sample(aid, sample);
        }
    }
    (
        StatusCode::OK,
        Json(IngestResponse {
            ok: true,
            agent_id,
        }),
    )
}

fn resolve_agent_id(s: &AppState, payload: &IngestPayload) -> Result<Option<i64>> {
    let pid = match payload.pid {
        Some(p) => p,
        None => return Ok(None),
    };
    if let Some(row) = s.db.find_agent_by_pid(pid)? {
        return Ok(Some(row.id));
    }
    let project_id = default_project(&s.db)?;
    let aid = s
        .db
        .insert_agent(project_id, &payload.kind, Some(pid), "user")?;
    Ok(Some(aid))
}

fn default_project(db: &Database) -> Result<i64> {
    // Fall back to cwd if no project is registered yet.
    let cwd = std::env::current_dir()?
        .display()
        .to_string();
    db.upsert_project(&cwd)
}

async fn rpc(State(s): State<AppState>, Json(req): Json<RpcRequest>) -> impl IntoResponse {
    let result = match req.method.as_str() {
        "register_project" => rpc_register_project(&s, &req.params),
        "register_agent" => rpc_register_agent(&s, &req.params),
        "list_agents" => rpc_list_agents(&s, &req.params),
        "stop_agent" => rpc_stop_agent(&s, &req.params),
        m => Err(anyhow::anyhow!("unknown method: {m}")),
    };
    let resp = match result {
        Ok(v) => RpcResponse {
            ok: true,
            result: Some(v),
            error: None,
        },
        Err(e) => RpcResponse {
            ok: false,
            result: None,
            error: Some(e.to_string()),
        },
    };
    (StatusCode::OK, Json(resp))
}

fn rpc_register_project(s: &AppState, p: &serde_json::Value) -> Result<serde_json::Value> {
    let root = p
        .get("root")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("root required"))?;
    let pid = s.db.upsert_project(root)?;
    Ok(json!({"project_id": pid, "root": root}))
}

fn rpc_register_agent(s: &AppState, p: &serde_json::Value) -> Result<serde_json::Value> {
    let kind = p
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("kind required"))?;
    let project_id = match p.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => default_project(&s.db)?,
    };
    let pid = p.get("pid").and_then(|v| v.as_i64());
    let spawned_by = p.get("spawned_by").and_then(|v| v.as_str()).unwrap_or("handoff");
    let aid = s.db.insert_agent(project_id, kind, pid, spawned_by)?;
    Ok(json!({"agent_id": aid}))
}

fn rpc_list_agents(s: &AppState, p: &serde_json::Value) -> Result<serde_json::Value> {
    let project_id = p.get("project_id").and_then(|v| v.as_i64());
    let summaries = s.db.list_agent_summaries(project_id)?;
    let agents: Vec<serde_json::Value> = summaries
        .into_iter()
        .map(|a| {
            json!({
                "id": a.id,
                "kind": a.kind,
                "pid": a.pid,
                "status": a.status,
                "spawned_by": a.spawned_by,
                "started_at": a.started_at,
                "tokens_remaining": a.tokens_remaining,
                "requests_remaining": a.requests_remaining,
                "tokens_reset_at": a.tokens_reset_at,
                "last_sample_ts": a.last_sample_ts,
                "total_requests": a.total_requests,
                "rate_limited_count": a.rate_limited_count,
                "last_429_at": a.last_429_at,
            })
        })
        .collect();
    Ok(json!({ "agents": agents }))
}

fn rpc_stop_agent(s: &AppState, p: &serde_json::Value) -> Result<serde_json::Value> {
    let aid = p
        .get("agent_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("agent_id required"))?;
    let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("stopped");
    s.db.mark_agent_stopped(aid, status)?;
    Ok(json!({"ok": true}))
}

pub async fn serve(state: AppState, addr: SocketAddr) -> Result<()> {
    let app = build_router(state);
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Hint to silence "unused import" when adapters list isn't referenced
/// (kept for forthcoming endpoints).
#[allow(dead_code)]
fn _adapters_keepalive() -> usize {
    all_adapters().len()
}
