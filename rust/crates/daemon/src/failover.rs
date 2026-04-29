//! Failover engine: subscribes to ingest events on an mpsc channel, decides
//! whether to fire a handoff, and (when `auto_spawn=true`) launches the next
//! agent in the chain.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use handoff_context::ContextEngine;
use handoff_critic::CriticRunner;
use handoff_policy::{load as load_policy, pick_next, should_trigger};
use handoff_storage::Database;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use crate::spawn::headless_spawn;

#[derive(Debug, Clone)]
pub struct RateEvent {
    pub agent_id: i64,
    pub kind: String,
    pub tokens_remaining: Option<i64>,
    pub requests_remaining: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct HandoffOutcome {
    pub handoff_id: i64,
    pub to_agent_id: Option<i64>,
    pub to_pid: Option<u32>,
    pub snapshot_path: String,
}

/// Default channel capacity. Tune in production.
pub const CHANNEL_CAP: usize = 256;

pub fn channel() -> (mpsc::Sender<RateEvent>, mpsc::Receiver<RateEvent>) {
    mpsc::channel(CHANNEL_CAP)
}

pub struct FailoverEngine {
    db: Arc<Database>,
    proxy_url: String,
    /// (from_agent_id, to_kind) pairs that already fired.
    fired: Mutex<HashSet<(i64, String)>>,
    /// Highest `tokens_remaining` ever seen per agent — proxy for "initial budget".
    initial_tokens: Mutex<HashMap<i64, i64>>,
}

struct ExecuteRequest<'a> {
    from_agent_id: Option<i64>,
    to_kind: &'a str,
    project_root: &'a Path,
    project_id: i64,
    reason: &'a str,
    auto_spawn: bool,
    summarize: bool,
    use_worktree: bool,
}

impl FailoverEngine {
    pub fn new(db: Arc<Database>, proxy_url: String) -> Self {
        Self {
            db,
            proxy_url,
            fired: Mutex::new(HashSet::new()),
            initial_tokens: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn the engine task and return its sender. Drop the sender to stop it.
    pub fn spawn(self: Arc<Self>) -> mpsc::Sender<RateEvent> {
        let (tx, mut rx) = channel();
        let me = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if let Err(e) = me.handle(ev).await {
                    warn!("failover handle error: {e}");
                }
            }
        });
        tx
    }

    pub async fn handle(&self, ev: RateEvent) -> Result<()> {
        if let Some(tok) = ev.tokens_remaining {
            let mut m = self.initial_tokens.lock().await;
            let prev = m.get(&ev.agent_id).copied().unwrap_or(0);
            if tok > prev {
                m.insert(ev.agent_id, tok);
            }
        }
        let project_id = match self.db.project_id_for_agent(ev.agent_id)? {
            Some(p) => p,
            None => return Ok(()),
        };
        let root = match self.db.project_root(project_id)? {
            Some(r) => r,
            None => return Ok(()),
        };
        let root = std::path::PathBuf::from(root);
        let policy = load_policy(&root).unwrap_or_default();
        let initial = self.initial_tokens.lock().await.get(&ev.agent_id).copied();
        let trigger = should_trigger(
            &policy.failover,
            ev.tokens_remaining,
            ev.requests_remaining,
            initial,
        );
        if !trigger.fired {
            return Ok(());
        }

        let samples: Vec<handoff_policy::RateSampleInput> = policy
            .failover
            .chain
            .iter()
            .filter_map(|kind| {
                self.db
                    .latest_rate_sample_for_kind(kind)
                    .ok()
                    .flatten()
                    .map(|s| handoff_policy::RateSampleInput {
                        kind: kind.clone(),
                        tokens_remaining: s.tokens_remaining.unwrap_or(0),
                        tokens_reset_at: s.tokens_reset_at,
                    })
            })
            .collect();

        let next_kind = match pick_next(&policy.failover.chain, &ev.kind, &samples) {
            Some(k) => k.to_string(),
            None => return Ok(()),
        };
        let key = (ev.agent_id, next_kind.clone());
        {
            let mut fired = self.fired.lock().await;
            if fired.contains(&key) {
                return Ok(());
            }
            fired.insert(key);
        }
        info!(
            "failover triggered: agent_id={} kind={} reason={} -> {}",
            ev.agent_id, ev.kind, trigger.reason, next_kind,
        );
        self.execute_inner(ExecuteRequest {
            from_agent_id: Some(ev.agent_id),
            to_kind: &next_kind,
            project_root: &root,
            project_id,
            reason: &trigger.reason,
            auto_spawn: policy.failover.auto_spawn,
            summarize: policy.failover.summarize,
            use_worktree: policy.failover.use_worktree,
        })
        .await?;
        Ok(())
    }

    /// Manual / RPC entry point. Returns the `HandoffOutcome` so the caller
    /// can render it.
    pub async fn execute(
        &self,
        from_agent_id: Option<i64>,
        to_kind: &str,
        project_root: &Path,
        project_id: i64,
        reason: &str,
        auto_spawn: bool,
    ) -> Result<HandoffOutcome> {
        let policy = load_policy(project_root).unwrap_or_default();
        self.execute_inner(ExecuteRequest {
            from_agent_id,
            to_kind,
            project_root,
            project_id,
            reason,
            auto_spawn,
            summarize: policy.failover.summarize,
            use_worktree: policy.failover.use_worktree,
        })
        .await
    }

    async fn execute_inner(&self, req: ExecuteRequest<'_>) -> Result<HandoffOutcome> {
        let ExecuteRequest {
            from_agent_id,
            to_kind,
            project_root,
            project_id,
            reason,
            auto_spawn,
            summarize,
            use_worktree,
        } = req;

        let engine = ContextEngine::new(project_root);
        let (mut snap, _md_path) = engine.snapshot(reason)?;
        // Ask the critic agent for a focused brief if configured. Failure
        // is non-fatal — we fall back to the verbatim brain dump.
        if summarize {
            let policy = handoff_policy::load(project_root).unwrap_or_default();
            if let Ok(runner) = CriticRunner::new(project_root) {
                let runner = runner
                    .with_agents(&policy.critic.worker_agent, &policy.critic.summarizer_agent)
                    .with_proxy(Some(self.proxy_url.clone()));
                match runner.summarize_for_handoff(reason).await {
                    Ok(brief) => snap.critic_brief = Some(brief),
                    Err(e) => warn!("summarizer failed; verbatim fallback: {e}"),
                }
            }
        }
        let (snap_path, _) = engine.write_snapshot(&snap)?;
        let snap_path_str = snap_path.display().to_string();

        let mut new_agent_id: Option<i64> = None;
        let mut new_pid: Option<u32> = None;
        if auto_spawn {
            // Insert agent record first to get an ID for the worktree
            let aid = self.db.insert_agent(
                project_id, to_kind, None, // PID unknown yet
                "handoff",
            )?;
            new_agent_id = Some(aid);

            let prompt = format!("Resuming work. Read this snapshot first: {}", snap_path_str);
            match headless_spawn(
                aid,
                to_kind,
                project_root,
                &prompt,
                &self.proxy_url,
                use_worktree,
            )
            .await
            {
                Ok(Some(child)) => {
                    new_pid = child.id();
                    if let Some(pid) = new_pid {
                        self.db.update_agent_pid(aid, pid as i64)?;
                    }
                }
                Ok(None) => {
                    info!("no headless form for kind={to_kind}; skipping spawn");
                }
                Err(e) => warn!("failover spawn failed: {e}"),
            }
        }

        let handoff_id =
            self.db
                .insert_handoff(from_agent_id, new_agent_id, reason, Some(&snap_path_str))?;
        Ok(HandoffOutcome {
            handoff_id,
            to_agent_id: new_agent_id,
            to_pid: new_pid,
            snapshot_path: snap_path_str,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dedup_per_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("t.db");
        let db = Arc::new(Database::open(&db_path).unwrap());
        let pid = db
            .upsert_project(&tmp.path().display().to_string())
            .unwrap();

        // Scaffold .handoff/ in the project root so snapshot/write succeed.
        std::fs::create_dir_all(tmp.path().join(".handoff").join("scratch")).unwrap();
        std::fs::write(
            tmp.path().join(".handoff").join("config.toml"),
            r#"[failover]
requests_remaining = 100
auto_spawn = false
chain = ["codex", "claude"]
"#,
        )
        .unwrap();
        std::fs::write(tmp.path().join(".handoff").join("brain.md"), "# brain").unwrap();

        let aid = db.insert_agent(pid, "claude", Some(999), "user").unwrap();
        let engine = Arc::new(FailoverEngine::new(
            db.clone(),
            "http://127.0.0.1:8080".into(),
        ));

        let ev = RateEvent {
            agent_id: aid,
            kind: "claude".into(),
            tokens_remaining: None,
            requests_remaining: Some(1),
        };
        engine.handle(ev.clone()).await.unwrap();
        engine.handle(ev).await.unwrap();

        // Only one handoff row should exist; query via a fresh connection.
        // Drop the in-memory db's reference so its Mutex doesn't fight us.
        drop(engine);
        drop(db);
        let n = count_handoffs(&db_path, aid);
        assert_eq!(n, 1);
    }

    fn count_handoffs(path: &std::path::Path, aid: i64) -> i64 {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.query_row::<i64, _, _>(
            "SELECT COUNT(*) FROM handoffs WHERE from_agent_id = ?1",
            rusqlite::params![aid],
            |r| r.get(0),
        )
        .unwrap()
    }
}
