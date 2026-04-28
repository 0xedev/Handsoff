//! HTTP daemon: /ingest for the proxy, /rpc for the CLI.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{State, Query},
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
use tokio::sync::mpsc::Sender;
use tracing::warn;

pub mod failover;
pub mod spawn;
pub mod worktree;

use failover::{FailoverEngine, RateEvent};

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub events: Sender<RateEvent>,
    pub failover: Arc<FailoverEngine>,
    pub brain_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AppState {
    /// Build an `AppState` plus spawn the failover engine task. Returns the
    /// state ready to mount under axum.
    pub fn bootstrap(db: Arc<Database>, proxy_url: String) -> Self {
        let engine = Arc::new(FailoverEngine::new(db.clone(), proxy_url));
        let events = engine.clone().spawn();
        AppState {
            db,
            events,
            failover: engine,
            brain_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
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
        .route("/simulate", post(simulate_limit))
        .route("/handoffs", get(list_handoffs_http))
        .route("/events", get(list_events_http))
        .route("/hook", post(hook))
        .route("/brain/append", post(brain_append))
        .route("/brain/edit", post(brain_edit))
        .with_state(state)
}

#[derive(serde::Deserialize)]
struct HookPayload {
    agent_pid: Option<i32>,
    tool_name: Option<String>,
    tool_input: Option<String>,
}

async fn hook(
    State(state): State<AppState>,
    Json(payload): Json<HookPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    if let Some(pid) = payload.agent_pid {
        if let Ok(Some(agent)) = state.db.find_agent_by_pid(pid as i64) {
            let _ = state.db.insert_activity(
                agent.id,
                payload.tool_name.as_deref().unwrap_or("unknown"),
                payload.tool_input.as_deref(),
            );
        }
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true})))
}

#[derive(serde::Deserialize, Default)]
struct PaginationParams {
    limit: Option<usize>,
}

async fn list_handoffs_http(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(5);
    let rows = state.db.list_handoffs_recent(limit as i64).unwrap_or_default();
    Json(serde_json::json!({"handoffs": rows}))
}

#[derive(serde::Deserialize, Default)]
struct EventsParams {
    since: Option<u64>,
}

async fn list_events_http(
    State(_state): State<AppState>,
    Query(_params): Query<EventsParams>,
) -> Json<serde_json::Value> {
    // Stub
    Json(serde_json::json!({"events": []}))
}

#[derive(serde::Deserialize)]
struct SimulatePayload {
    agent_id: i64,
    #[serde(default)]
    tokens: Option<i64>,
    #[serde(default)]
    requests: Option<i64>,
}

async fn simulate_limit(
    State(state): State<AppState>,
    Json(payload): Json<SimulatePayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let ev = failover::RateEvent {
        agent_id: payload.agent_id,
        kind: "simulated".to_string(),
        tokens_remaining: payload.tokens,
        requests_remaining: payload.requests,
    };
    state.events.send(ev).await.ok();
    (StatusCode::OK, Json(serde_json::json!({"ok": true})))
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
            let ev = RateEvent {
                agent_id: aid,
                kind: payload.kind.clone(),
                tokens_remaining: sample.tokens_remaining,
                requests_remaining: sample.requests_remaining,
            };
            let _ = s.events.try_send(ev);
        } else if payload.status_code == 429 {
            // Opaque-provider failover trigger.
            let ev = RateEvent {
                agent_id: aid,
                kind: payload.kind.clone(),
                tokens_remaining: Some(0),
                requests_remaining: Some(0),
            };
            let _ = s.events.try_send(ev);
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
    let cwd = std::env::current_dir()?.display().to_string();
    db.upsert_project(&cwd)
}

async fn rpc(State(s): State<AppState>, Json(req): Json<RpcRequest>) -> impl IntoResponse {
    let result = match req.method.as_str() {
        "register_project" => rpc_register_project(&s, &req.params),
        "register_agent" => rpc_register_agent(&s, &req.params),
        "list_agents" => rpc_list_agents(&s, &req.params),
        "stop_agent" => rpc_stop_agent(&s, &req.params),
        "attach_agent" => rpc_attach_agent(&s, &req.params),
        "handoff" => return Json(rpc_response(rpc_handoff(&s, req.params).await)).into_response(),
        "record_critic_run" => rpc_record_critic_run(&s, &req.params),
        m => Err(anyhow::anyhow!("unknown method: {m}")),
    };
    Json(rpc_response(result)).into_response()
}

fn rpc_response(result: Result<serde_json::Value>) -> RpcResponse {
    match result {
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
    }
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
    let spawned_by = p
        .get("spawned_by")
        .and_then(|v| v.as_str())
        .unwrap_or("handoff");
    let aid = s.db.insert_agent(project_id, kind, pid, spawned_by)?;
    Ok(json!({"agent_id": aid}))
}

fn rpc_attach_agent(s: &AppState, p: &serde_json::Value) -> Result<serde_json::Value> {
    let kind = p
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("kind required"))?;
    let pid = p
        .get("pid")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("pid required"))?;
    let project_id = match p.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => default_project(&s.db)?,
    };
    let aid = s.db.insert_agent(project_id, kind, Some(pid), "user")?;
    Ok(json!({"agent_id": aid, "project_id": project_id}))
}

async fn rpc_handoff(s: &AppState, p: serde_json::Value) -> Result<serde_json::Value> {
    let to_kind = p
        .get("to_kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("to_kind required"))?
        .to_string();
    let from_agent_id = p.get("from_agent_id").and_then(|v| v.as_i64());
    let auto_spawn = p
        .get("auto_spawn")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let reason = p
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("manual")
        .to_string();
    let project_id = match p.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => match from_agent_id.and_then(|aid| s.db.project_id_for_agent(aid).ok().flatten()) {
            Some(id) => id,
            None => default_project(&s.db)?,
        },
    };
    let root = s
        .db
        .project_root(project_id)?
        .ok_or_else(|| anyhow::anyhow!("project_id={project_id} has no root"))?;
    let outcome = s
        .failover
        .execute(
            from_agent_id,
            &to_kind,
            &PathBuf::from(&root),
            project_id,
            &reason,
            auto_spawn,
        )
        .await?;
    Ok(json!({
        "handoff_id": outcome.handoff_id,
        "to_agent_id": outcome.to_agent_id,
        "to_pid": outcome.to_pid,
        "snapshot_path": outcome.snapshot_path,
    }))
}

fn rpc_record_critic_run(s: &AppState, p: &serde_json::Value) -> Result<serde_json::Value> {
    let worker_model = p
        .get("worker_model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("worker_model required"))?;
    let critic_model = p
        .get("critic_model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("critic_model required"))?;
    let verdict = p
        .get("verdict")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("verdict required"))?;
    let project_id = match p.get("project_id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => default_project(&s.db)?,
    };
    let worker_tokens = p.get("worker_tokens").and_then(|v| v.as_u64());
    let critic_tokens = p.get("critic_tokens").and_then(|v| v.as_u64());
    let notes = p.get("notes").and_then(|v| v.as_str());
    let id = s.db.insert_critic_run(
        project_id,
        worker_model,
        critic_model,
        worker_tokens,
        critic_tokens,
        verdict,
        notes,
    )?;
    Ok(json!({"critic_run_id": id}))
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
    let status = p
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("stopped");
    s.db.mark_agent_stopped(aid, status)?;
    Ok(json!({"ok": true}))
}

pub async fn serve(state: AppState, addr: SocketAddr) -> Result<()> {
    let app = build_router(state);
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Deserialize)]
struct BrainAppend {
    project_root: String,
    text: String,
}

#[derive(Deserialize)]
struct BrainEdit {
    project_root: String,
    content: String,
}

async fn brain_append(
    State(state): State<AppState>,
    Json(payload): Json<BrainAppend>,
) -> impl IntoResponse {
    let _lock = state.brain_lock.lock().await;
    let path = std::path::Path::new(&payload.project_root)
        .join(".handoff")
        .join("brain.md");
    
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path) {
            Ok(f) => f,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to open brain.md: {e}")).into_response(),
        };
    
    if let Err(e) = writeln!(file, "\n{}", payload.text) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to write to brain.md: {e}")).into_response();
    }

    StatusCode::OK.into_response()
}

async fn brain_edit(
    State(state): State<AppState>,
    Json(payload): Json<BrainEdit>,
) -> impl IntoResponse {
    let _lock = state.brain_lock.lock().await;
    let path = std::path::Path::new(&payload.project_root)
        .join(".handoff")
        .join("brain.md");
    
    if let Err(e) = std::fs::write(&path, &payload.content) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("failed to write brain.md: {e}")).into_response();
    }

    StatusCode::OK.into_response()
}

#[allow(dead_code)]
fn _adapters_keepalive() -> usize {
    all_adapters().len()
}

