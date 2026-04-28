//! SQLite persistence. Schema is byte-compatible with the v0.x Python
//! version so an existing `~/.handoff/state.db` migrates without ceremony.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::Utc;
use handoff_common::RateSample;
use rusqlite::{params, Connection, OptionalExtension, Row};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS projects (
    id          INTEGER PRIMARY KEY,
    root_path   TEXT UNIQUE NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS agents (
    id          INTEGER PRIMARY KEY,
    project_id  INTEGER REFERENCES projects(id),
    kind        TEXT NOT NULL,
    pid         INTEGER,
    spawned_by  TEXT NOT NULL,
    status      TEXT NOT NULL,
    started_at  INTEGER NOT NULL,
    ended_at    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_agents_project ON agents(project_id);
CREATE INDEX IF NOT EXISTS idx_agents_pid ON agents(pid);

CREATE TABLE IF NOT EXISTS rate_samples (
    id                  INTEGER PRIMARY KEY,
    agent_id            INTEGER REFERENCES agents(id),
    ts                  INTEGER NOT NULL,
    provider            TEXT NOT NULL,
    tokens_remaining    INTEGER,
    requests_remaining  INTEGER,
    tokens_reset_at     INTEGER,
    requests_reset_at   INTEGER,
    raw_headers         TEXT
);
CREATE INDEX IF NOT EXISTS idx_rate_agent_ts ON rate_samples(agent_id, ts);

CREATE TABLE IF NOT EXISTS handoffs (
    id                      INTEGER PRIMARY KEY,
    from_agent_id           INTEGER REFERENCES agents(id),
    to_agent_id             INTEGER REFERENCES agents(id),
    reason                  TEXT NOT NULL,
    ts                      INTEGER NOT NULL,
    context_snapshot_path   TEXT
);

CREATE TABLE IF NOT EXISTS critic_runs (
    id              INTEGER PRIMARY KEY,
    project_id      INTEGER REFERENCES projects(id),
    ts              INTEGER NOT NULL,
    worker_model    TEXT NOT NULL,
    critic_model    TEXT NOT NULL,
    worker_tokens   INTEGER,
    critic_tokens   INTEGER,
    verdict         TEXT,
    notes           TEXT
);

CREATE TABLE IF NOT EXISTS request_counts (
    agent_id        INTEGER PRIMARY KEY REFERENCES agents(id),
    total           INTEGER NOT NULL DEFAULT 0,
    rate_limited    INTEGER NOT NULL DEFAULT 0,
    last_request_at INTEGER,
    last_429_at     INTEGER
);

CREATE TABLE IF NOT EXISTS agent_activity (
    id          INTEGER PRIMARY KEY,
    agent_id    INTEGER NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    ts          INTEGER NOT NULL,
    tool_name   TEXT,
    tool_input  TEXT
);
CREATE INDEX IF NOT EXISTS idx_activity_agent ON agent_activity(agent_id, ts);
"#;

pub struct Database {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone)]
pub struct AgentRow {
    pub id: i64,
    pub project_id: i64,
    pub kind: String,
    pub pid: Option<i64>,
    pub spawned_by: String,
    pub status: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentSummary {
    pub id: i64,
    pub kind: String,
    pub pid: Option<i64>,
    pub status: String,
    pub spawned_by: String,
    pub started_at: i64,
    pub tokens_remaining: Option<i64>,
    pub requests_remaining: Option<i64>,
    pub tokens_reset_at: Option<i64>,
    pub last_sample_ts: Option<i64>,
    pub total_requests: i64,
    pub rate_limited_count: i64,
    pub last_429_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct RateSampleRow {
    pub tokens_remaining: Option<i64>,
    pub tokens_reset_at: Option<i64>,
}


#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HandoffRow {
    pub id: i64,
    pub from_agent_id: Option<i64>,
    pub to_agent_id: Option<i64>,
    pub reason: String,
    pub ts: i64,
    pub context_snapshot_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReplayData {
    pub handoff: HandoffRow,
    pub from_agent: Option<AgentSummary>,
    pub to_agent: Option<AgentSummary>,
    pub snapshot_content: Option<String>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DailyStat {
    pub date: String,
    pub kind: String,
    pub total_requests: i64,
    pub avg_tokens_remaining: Option<f64>,
    pub handoff_count: i64,
}



impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn upsert_project(&self, root_path: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO projects(root_path, created_at) VALUES(?1, ?2) \
             ON CONFLICT(root_path) DO UPDATE SET root_path=excluded.root_path",
            params![root_path, now],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM projects WHERE root_path = ?1",
            params![root_path],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn insert_agent(
        &self,
        project_id: i64,
        kind: &str,
        pid: Option<i64>,
        spawned_by: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO agents(project_id, kind, pid, spawned_by, status, started_at) \
             VALUES(?1, ?2, ?3, ?4, 'running', ?5)",
            params![project_id, kind, pid, spawned_by, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn insert_activity(&self, agent_id: i64, tool_name: &str, tool_input: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO agent_activity (agent_id, ts, tool_name, tool_input) VALUES (?1, ?2, ?3, ?4)",
            params![agent_id, chrono::Utc::now().timestamp(), tool_name, tool_input],
        )?;
        Ok(())
    }

    pub fn update_agent_pid(&self, id: i64, pid: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE agents SET pid = ?1 WHERE id = ?2",
            params![pid, id],
        )?;
        Ok(())
    }

    pub fn mark_agent_stopped(&self, agent_id: i64, status: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp();
        conn.execute(
            "UPDATE agents SET status = ?1, ended_at = ?2 WHERE id = ?3",
            params![status, now, agent_id],
        )?;
        Ok(())
    }

    pub fn find_agent_by_pid(&self, pid: i64) -> Result<Option<AgentRow>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT id, project_id, kind, pid, spawned_by, status, started_at, ended_at \
                 FROM agents WHERE pid = ?1 AND status = 'running' \
                 ORDER BY started_at DESC LIMIT 1",
                params![pid],
                map_agent_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn latest_rate_sample_for_kind(&self, kind: &str) -> Result<Option<RateSampleRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT rs.tokens_remaining, rs.tokens_reset_at FROM rate_samples rs \
             JOIN agents a ON a.id = rs.agent_id \
             WHERE a.kind = ?1 \
             ORDER BY rs.ts DESC LIMIT 1",
            params![kind],
            |row| {
                Ok(RateSampleRow {
                    tokens_remaining: row.get(0)?,
                    tokens_reset_at: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn project_root(&self, project_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT root_path FROM projects WHERE id = ?1",
                params![project_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    pub fn project_id_for_agent(&self, agent_id: i64) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT project_id FROM agents WHERE id = ?1",
                params![agent_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
    }

    pub fn insert_rate_sample(&self, agent_id: i64, sample: &RateSample) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp();
        let raw = serde_json::to_string(&sample.raw_headers)?;
        conn.execute(
            "INSERT INTO rate_samples(\
             agent_id, ts, provider, tokens_remaining, requests_remaining, \
             tokens_reset_at, requests_reset_at, raw_headers) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                agent_id,
                now,
                sample.provider,
                sample.tokens_remaining,
                sample.requests_remaining,
                sample.tokens_reset_at,
                sample.requests_reset_at,
                raw,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn bump_request_count(&self, agent_id: i64, status_code: u16) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp();
        let is_429: i64 = if status_code == 429 { 1 } else { 0 };
        conn.execute(
            "INSERT INTO request_counts(agent_id, total, rate_limited, last_request_at, last_429_at) \
             VALUES(?1, 1, ?2, ?3, ?4) \
             ON CONFLICT(agent_id) DO UPDATE SET \
                total = total + 1, \
                rate_limited = rate_limited + excluded.rate_limited, \
                last_request_at = excluded.last_request_at, \
                last_429_at = COALESCE(excluded.last_429_at, last_429_at)",
            params![agent_id, is_429, now, if is_429 == 1 { Some(now) } else { None }],
        )?;
        Ok(())
    }

    pub fn list_agent_summaries(&self, project_id: Option<i64>) -> Result<Vec<AgentSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match project_id {
            Some(_) => conn.prepare(
                "SELECT id, kind, pid, spawned_by, status, started_at \
                 FROM agents WHERE project_id = ?1 AND status = 'running' \
                 ORDER BY started_at DESC",
            )?,
            None => conn.prepare(
                "SELECT id, kind, pid, spawned_by, status, started_at \
                 FROM agents WHERE status = 'running' ORDER BY started_at DESC",
            )?,
        };
        let rows = match project_id {
            Some(pid) => stmt.query(params![pid])?,
            None => stmt.query([])?,
        };
        let mut out = Vec::new();
        let mut iter = rows;
        while let Some(r) = iter.next()? {
            let id: i64 = r.get(0)?;
            let mut s = AgentSummary {
                id,
                kind: r.get(1)?,
                pid: r.get(2)?,
                spawned_by: r.get(3)?,
                status: r.get(4)?,
                started_at: r.get(5)?,
                ..Default::default()
            };
            // latest sample
            if let Ok(latest) = conn.query_row(
                "SELECT tokens_remaining, requests_remaining, tokens_reset_at, ts \
                 FROM rate_samples WHERE agent_id = ?1 ORDER BY ts DESC LIMIT 1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            ) {
                s.tokens_remaining = latest.0;
                s.requests_remaining = latest.1;
                s.tokens_reset_at = latest.2;
                s.last_sample_ts = Some(latest.3);
            }
            // request counts
            if let Ok(counts) = conn.query_row(
                "SELECT total, rate_limited, last_429_at FROM request_counts WHERE agent_id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            ) {
                s.total_requests = counts.0;
                s.rate_limited_count = counts.1;
                s.last_429_at = counts.2;
            }
            out.push(s);
        }
        Ok(out)
    }

    pub fn insert_handoff(
        &self,
        from_agent_id: Option<i64>,
        to_agent_id: Option<i64>,
        reason: &str,
        snapshot_path: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO handoffs(from_agent_id, to_agent_id, reason, ts, context_snapshot_path) \
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![from_agent_id, to_agent_id, reason, now, snapshot_path],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn get_handoff(&self, id: i64) -> Result<HandoffRow> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, from_agent_id, to_agent_id, reason, ts, context_snapshot_path \
             FROM handoffs WHERE id = ?"
        )?;
        let row = stmt.query_row([id], |row| {
            Ok(HandoffRow {
                id: row.get(0)?,
                from_agent_id: row.get(1)?,
                to_agent_id: row.get(2)?,
                reason: row.get(3)?,
                ts: row.get(4)?,
                context_snapshot_path: row.get(5)?,
            })
        })?;
        Ok(row)
    }
    pub fn get_agent(&self, id: i64) -> Result<AgentSummary> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, pid, status, spawned_by, started_at, tokens_remaining, requests_remaining, \
             tokens_reset_at, last_sample_ts, total_requests, rate_limited_count, last_429_at \
             FROM agents WHERE id = ?"
        )?;
        let row = stmt.query_row([id], |row| {
            Ok(AgentSummary {
                id: row.get(0)?,
                kind: row.get(1)?,
                pid: row.get(2)?,
                status: row.get(3)?,
                spawned_by: row.get(4)?,
                started_at: row.get(5)?,
                tokens_remaining: row.get(6)?,
                requests_remaining: row.get(7)?,
                tokens_reset_at: row.get(8)?,
                last_sample_ts: row.get(9)?,
                total_requests: row.get(10)?,
                rate_limited_count: row.get(11)?,
                last_429_at: row.get(12)?,
            })
        })?;
        Ok(row)
    }

    pub fn get_replay_data(&self, handoff_id: i64) -> Result<ReplayData> {
        let h = self.get_handoff(handoff_id)?;
        let from = h.from_agent_id.and_then(|id| self.get_agent(id).ok());
        let to = h.to_agent_id.and_then(|id| self.get_agent(id).ok());
        let snap = h.context_snapshot_path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());

        Ok(ReplayData {
            handoff: h,
            from_agent: from,
            to_agent: to,
            snapshot_content: snap,
        })
    }

    pub fn daily_stats(&self, days: u32) -> Result<Vec<DailyStat>> {
        let conn = self.conn.lock().unwrap();
        let since = chrono::Utc::now().timestamp() - (days as i64 * 86400);
        let mut stmt = conn.prepare(
            "SELECT date(rs.ts, 'unixepoch') as d, \
                    a.kind, \
                    COUNT(*) as total, \
                    AVG(rs.tokens_remaining) as avg_tok, \
                    (SELECT COUNT(*) FROM handoffs h JOIN agents fa ON fa.id = h.from_agent_id WHERE fa.kind = a.kind AND date(h.ts, 'unixepoch') = date(rs.ts, 'unixepoch')) as hc \
             FROM rate_samples rs \
             JOIN agents a ON a.id = rs.agent_id \
             WHERE rs.ts > ?1 \
             GROUP BY d, a.kind \
             ORDER BY d DESC"
        )?;
        let rows = stmt.query_map([since], |row| {
            Ok(DailyStat {
                date: row.get(0)?,
                kind: row.get(1)?,
                total_requests: row.get(2)?,
                avg_tokens_remaining: row.get(3)?,
                handoff_count: row.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn list_handoffs_recent(&self, limit: i64) -> Result<Vec<HandoffRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, from_agent_id, to_agent_id, reason, ts, context_snapshot_path \
             FROM handoffs ORDER BY ts DESC LIMIT ?1"
        )?;
        let rows = stmt.query(rusqlite::params![limit])?;
        let mut out = Vec::new();
        let mut iter = rows;
        while let Some(r) = iter.next()? {
            out.push(HandoffRow {
                id: r.get(0)?,
                from_agent_id: r.get(1)?,
                to_agent_id: r.get(2)?,
                reason: r.get(3)?,
                ts: r.get(4)?,
                context_snapshot_path: r.get(5)?,
            });
        }
        Ok(out)
    }

    pub fn insert_critic_run(
        &self,
        project_id: i64,
        worker_model: &str,
        critic_model: &str,
        worker_tokens: Option<u64>,
        critic_tokens: Option<u64>,
        verdict: &str,
        notes: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO critic_runs(\
             project_id, ts, worker_model, critic_model, \
             worker_tokens, critic_tokens, verdict, notes) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                project_id,
                now,
                worker_model,
                critic_model,
                worker_tokens.map(|x| x as i64),
                critic_tokens.map(|x| x as i64),
                verdict,
                notes,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }
}

fn map_agent_row(row: &Row) -> rusqlite::Result<AgentRow> {
    Ok(AgentRow {
        id: row.get(0)?,
        project_id: row.get(1)?,
        kind: row.get(2)?,
        pid: row.get(3)?,
        spawned_by: row.get(4)?,
        status: row.get(5)?,
        started_at: row.get(6)?,
        ended_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        let f = tempfile::NamedTempFile::new().unwrap();
        Database::open(f.path()).unwrap()
    }

    #[test]
    fn upsert_idempotent() {
        let d = db();
        let a = d.upsert_project("/x").unwrap();
        let b = d.upsert_project("/x").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn agent_round_trip() {
        let d = db();
        let p = d.upsert_project("/y").unwrap();
        let aid = d.insert_agent(p, "claude", Some(123), "user").unwrap();
        assert!(aid > 0);
        let s = handoff_common::RateSample {
            provider: "anthropic".into(),
            tokens_remaining: Some(500),
            requests_remaining: Some(50),
            ..Default::default()
        };
        d.insert_rate_sample(aid, &s).unwrap();
        d.bump_request_count(aid, 200).unwrap();
        d.bump_request_count(aid, 429).unwrap();
        let summaries = d.list_agent_summaries(Some(p)).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].tokens_remaining, Some(500));
        assert_eq!(summaries[0].total_requests, 2);
        assert_eq!(summaries[0].rate_limited_count, 1);
    }

    #[test]
    fn find_agent_by_pid() {
        let d = db();
        let p = d.upsert_project("/z").unwrap();
        let aid = d.insert_agent(p, "codex", Some(777), "handoff").unwrap();
        let row = d.find_agent_by_pid(777).unwrap().unwrap();
        assert_eq!(row.id, aid);
        assert_eq!(row.kind, "codex");
    }
}
