use axum::http::StatusCode;
use reqwest::Client;
use std::sync::Arc;
use tokio::net::TcpListener;
use handoff_daemon::AppState;
use handoff_storage::Database;

#[tokio::test]
async fn test_failover_e2e() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("state.db");
    let storage = Arc::new(Database::open(&db_path).unwrap());
    
    let project_root = tmp.path().to_string_lossy().to_string();
    let pid = storage.upsert_project(&project_root).unwrap();
    
    std::fs::create_dir_all(tmp.path().join(".handoff").join("scratch")).unwrap();
    std::fs::write(
        tmp.path().join(".handoff").join("config.toml"),
        r#"[failover]
requests_remaining = 100
auto_spawn = false
chain = ["simulated", "codex"]
"#,
    ).unwrap();
    std::fs::write(tmp.path().join(".handoff").join("brain.md"), "# brain").unwrap();

    let aid = storage.insert_agent(pid, "simulated", Some(1234), "user").unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = AppState::bootstrap(storage.clone(), "http://localhost:8080".into());
    let app = handoff_daemon::build_router(state);
    
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("http://{}/simulate", addr);
    let res = Client::new()
        .post(&url)
        .json(&serde_json::json!({
            "agent_id": aid,
            "tokens": 0,
            "requests": 0
        }))
        .send()
        .await
        .unwrap();
        
    assert_eq!(res.status(), StatusCode::OK);

    // Poll for the handoff row instead of sleeping a fixed duration.
    // The failover engine processes events asynchronously; we retry every 50ms
    // for up to 5 seconds before failing.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let mut count = 0i64;
    for _ in 0..100 {
        count = conn
            .query_row(
                "SELECT COUNT(*) FROM handoffs WHERE from_agent_id = ?1",
                rusqlite::params![aid],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if count >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert_eq!(count, 1, "Handoff should have been recorded within 5s");
}
