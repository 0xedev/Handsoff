use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use handoff_daemon::{build_router, AppState};
use handoff_storage::Database;
use tower::ServiceExt;

#[tokio::test]
async fn record_critic_run_accepts_agent_field_names() {
    let db = tempfile::NamedTempFile::new().unwrap();
    let storage = Arc::new(Database::open(db.path()).unwrap());
    let app = build_router(AppState::bootstrap(
        storage,
        "http://localhost:8080".to_string(),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/rpc")
                .header("Content-Type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "method": "record_critic_run",
                        "params": {
                            "worker_agent": "claude",
                            "critic_agent": "codex",
                            "verdict": "APPROVED",
                            "notes": "ok"
                        }
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["ok"], true);

    let conn = rusqlite::Connection::open(db.path()).unwrap();
    let row = conn
        .query_row(
            "SELECT worker_model, critic_model, verdict, notes FROM critic_runs LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, "claude");
    assert_eq!(row.1, "codex");
    assert_eq!(row.2, "APPROVED");
    assert_eq!(row.3.as_deref(), Some("ok"));
}
