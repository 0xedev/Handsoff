use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use handoff_daemon::build_router;
use handoff_daemon::AppState;
use handoff_storage::Database;
use std::sync::Arc;

#[tokio::test]
async fn simulate_limit_returns_ok() {
    let db = tempfile::NamedTempFile::new().unwrap();
    let storage = Arc::new(Database::open(db.path()).unwrap());
    let state = AppState::bootstrap(storage, "http://localhost:8080".to_string());
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/simulate")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "agent_id": 1,
                        "tokens": 0,
                        "requests": 0
                    })).unwrap()
                ))
                .unwrap()
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
