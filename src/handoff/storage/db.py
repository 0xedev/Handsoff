from __future__ import annotations

import json
import sqlite3
import time
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterator, Optional

from handoff.paths import db_path

SCHEMA = """
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
    spawned_by  TEXT NOT NULL,        -- 'user' | 'handoff'
    status      TEXT NOT NULL,        -- 'running' | 'stopped' | 'failed'
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

-- v0.2: Copilot has no per-request rate-limit headers, so we track raw
-- request + 429 counts per agent for any provider where parse_headers()
-- returns None.
CREATE TABLE IF NOT EXISTS request_counts (
    agent_id        INTEGER PRIMARY KEY REFERENCES agents(id),
    total           INTEGER NOT NULL DEFAULT 0,
    rate_limited    INTEGER NOT NULL DEFAULT 0,
    last_request_at INTEGER,
    last_429_at     INTEGER
);
"""


@dataclass
class RateSample:
    provider: str
    tokens_remaining: Optional[int] = None
    requests_remaining: Optional[int] = None
    tokens_reset_at: Optional[int] = None
    requests_reset_at: Optional[int] = None
    raw_headers: dict[str, str] = field(default_factory=dict)


class Database:
    def __init__(self, path: Path):
        self.path = path
        self._conn = sqlite3.connect(str(path), isolation_level=None, check_same_thread=False)
        self._conn.row_factory = sqlite3.Row
        self._conn.execute("PRAGMA journal_mode=WAL;")
        self._conn.execute("PRAGMA foreign_keys=ON;")
        self._conn.executescript(SCHEMA)

    @contextmanager
    def tx(self) -> Iterator[sqlite3.Connection]:
        try:
            self._conn.execute("BEGIN;")
            yield self._conn
            self._conn.execute("COMMIT;")
        except Exception:
            self._conn.execute("ROLLBACK;")
            raise

    def upsert_project(self, root_path: Path) -> int:
        now = int(time.time())
        cur = self._conn.execute(
            "INSERT INTO projects(root_path, created_at) VALUES(?, ?) "
            "ON CONFLICT(root_path) DO UPDATE SET root_path=excluded.root_path "
            "RETURNING id",
            (str(root_path), now),
        )
        return cur.fetchone()["id"]

    def insert_agent(
        self,
        *,
        project_id: int,
        kind: str,
        pid: Optional[int],
        spawned_by: str,
    ) -> int:
        now = int(time.time())
        cur = self._conn.execute(
            "INSERT INTO agents(project_id, kind, pid, spawned_by, status, started_at) "
            "VALUES(?, ?, ?, ?, 'running', ?) RETURNING id",
            (project_id, kind, pid, spawned_by, now),
        )
        return cur.fetchone()["id"]

    def mark_agent_stopped(self, agent_id: int, status: str = "stopped") -> None:
        self._conn.execute(
            "UPDATE agents SET status=?, ended_at=? WHERE id=?",
            (status, int(time.time()), agent_id),
        )

    def find_agent_by_pid(self, pid: int) -> Optional[sqlite3.Row]:
        cur = self._conn.execute(
            "SELECT * FROM agents WHERE pid=? AND status='running' "
            "ORDER BY started_at DESC LIMIT 1",
            (pid,),
        )
        return cur.fetchone()

    def list_agents(self, project_id: Optional[int] = None) -> list[sqlite3.Row]:
        if project_id is None:
            cur = self._conn.execute(
                "SELECT * FROM agents WHERE status='running' ORDER BY started_at DESC"
            )
        else:
            cur = self._conn.execute(
                "SELECT * FROM agents WHERE project_id=? AND status='running' "
                "ORDER BY started_at DESC",
                (project_id,),
            )
        return cur.fetchall()

    def insert_rate_sample(self, agent_id: int, sample: RateSample) -> int:
        cur = self._conn.execute(
            "INSERT INTO rate_samples("
            "agent_id, ts, provider, tokens_remaining, requests_remaining, "
            "tokens_reset_at, requests_reset_at, raw_headers"
            ") VALUES(?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
            (
                agent_id,
                int(time.time()),
                sample.provider,
                sample.tokens_remaining,
                sample.requests_remaining,
                sample.tokens_reset_at,
                sample.requests_reset_at,
                json.dumps(sample.raw_headers),
            ),
        )
        return cur.fetchone()["id"]

    def latest_sample_for_agent(self, agent_id: int) -> Optional[sqlite3.Row]:
        cur = self._conn.execute(
            "SELECT * FROM rate_samples WHERE agent_id=? ORDER BY ts DESC LIMIT 1",
            (agent_id,),
        )
        return cur.fetchone()

    def bump_request_count(self, agent_id: int, status_code: int) -> None:
        now = int(time.time())
        is_429 = 1 if status_code == 429 else 0
        self._conn.execute(
            "INSERT INTO request_counts(agent_id, total, rate_limited, last_request_at, last_429_at) "
            "VALUES(?, 1, ?, ?, ?) "
            "ON CONFLICT(agent_id) DO UPDATE SET "
            "  total = total + 1, "
            "  rate_limited = rate_limited + excluded.rate_limited, "
            "  last_request_at = excluded.last_request_at, "
            "  last_429_at = COALESCE(excluded.last_429_at, last_429_at)",
            (agent_id, is_429, now, now if is_429 else None),
        )

    def request_count_for_agent(self, agent_id: int) -> Optional[sqlite3.Row]:
        cur = self._conn.execute(
            "SELECT * FROM request_counts WHERE agent_id=?", (agent_id,)
        )
        return cur.fetchone()

    def project_root(self, project_id: int) -> Optional[str]:
        cur = self._conn.execute(
            "SELECT root_path FROM projects WHERE id=?", (project_id,)
        )
        row = cur.fetchone()
        return row["root_path"] if row else None

    def project_id_for_agent(self, agent_id: int) -> Optional[int]:
        cur = self._conn.execute(
            "SELECT project_id FROM agents WHERE id=?", (agent_id,)
        )
        row = cur.fetchone()
        return row["project_id"] if row else None

    def insert_handoff(
        self,
        *,
        from_agent_id: Optional[int],
        to_agent_id: Optional[int],
        reason: str,
        snapshot_path: Optional[str] = None,
    ) -> int:
        cur = self._conn.execute(
            "INSERT INTO handoffs(from_agent_id, to_agent_id, reason, ts, context_snapshot_path) "
            "VALUES(?, ?, ?, ?, ?) RETURNING id",
            (from_agent_id, to_agent_id, reason, int(time.time()), snapshot_path),
        )
        return cur.fetchone()["id"]

    def insert_critic_run(
        self,
        *,
        project_id: int,
        worker_model: str,
        critic_model: str,
        worker_tokens: Optional[int],
        critic_tokens: Optional[int],
        verdict: str,
        notes: Optional[str],
    ) -> int:
        cur = self._conn.execute(
            "INSERT INTO critic_runs("
            "project_id, ts, worker_model, critic_model, "
            "worker_tokens, critic_tokens, verdict, notes"
            ") VALUES(?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
            (
                project_id,
                int(time.time()),
                worker_model,
                critic_model,
                worker_tokens,
                critic_tokens,
                verdict,
                notes,
            ),
        )
        return cur.fetchone()["id"]

    def close(self) -> None:
        self._conn.close()


def open_db(path: Optional[Path] = None) -> Database:
    return Database(path or db_path())
