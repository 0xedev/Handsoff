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

    def close(self) -> None:
        self._conn.close()


def open_db(path: Optional[Path] = None) -> Database:
    return Database(path or db_path())
