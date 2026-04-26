"""FastAPI daemon: ingest endpoint for the proxy + JSON-RPC for the CLI.

Both the proxy and the CLI talk to this daemon. The proxy POSTs samples to
/ingest. The CLI calls /rpc with a method name + params.
"""

from __future__ import annotations

import logging
import os
import time
from pathlib import Path
from typing import Any, Optional

from fastapi import FastAPI
from pydantic import BaseModel

from handoff.daemon.eventbus import EventBus
from handoff.storage.db import RateSample, open_db

log = logging.getLogger("handoff.daemon")

DEFAULT_INGEST_PORT = int(os.environ.get("HANDOFF_DAEMON_PORT", "7879"))


# --- Models ---------------------------------------------------------------


class IngestPayload(BaseModel):
    kind: str
    host: str
    status_code: int
    pid: Optional[int] = None
    sample: Optional[dict] = None


class RpcRequest(BaseModel):
    method: str
    params: dict[str, Any] = {}


# --- App ------------------------------------------------------------------


def build_app() -> FastAPI:
    app = FastAPI(title="handoffd", version="0.1.0")
    db = open_db()
    bus = EventBus()
    app.state.db = db
    app.state.bus = bus

    @app.post("/ingest")
    def ingest(payload: IngestPayload) -> dict:
        agent_id: Optional[int] = None
        if payload.pid is not None:
            row = db.find_agent_by_pid(payload.pid)
            if row is None:
                # Auto-register: attach this PID to the most recent project (or 0)
                project_id = _default_project(db)
                agent_id = db.insert_agent(
                    project_id=project_id,
                    kind=payload.kind,
                    pid=payload.pid,
                    spawned_by="user",  # auto-detected
                )
            else:
                agent_id = row["id"]
        if payload.sample and agent_id is not None:
            sample = RateSample(**payload.sample)
            db.insert_rate_sample(agent_id, sample)
            bus.publish(
                "RateLimitUpdated",
                {
                    "agent_id": agent_id,
                    "kind": payload.kind,
                    "tokens_remaining": sample.tokens_remaining,
                    "requests_remaining": sample.requests_remaining,
                    "ts": int(time.time()),
                },
            )
        return {"ok": True, "agent_id": agent_id}

    @app.post("/rpc")
    def rpc(req: RpcRequest) -> dict:
        handler = RPC_METHODS.get(req.method)
        if handler is None:
            return {"ok": False, "error": f"unknown method: {req.method}"}
        try:
            result = handler(app, req.params)
            return {"ok": True, "result": result}
        except Exception as e:  # noqa: BLE001
            log.exception("rpc error")
            return {"ok": False, "error": str(e)}

    @app.get("/health")
    def health() -> dict:
        return {"ok": True, "ts": int(time.time())}

    return app


def _default_project(db) -> int:
    cur = db._conn.execute("SELECT id FROM projects ORDER BY id DESC LIMIT 1")
    row = cur.fetchone()
    if row:
        return row["id"]
    return db.upsert_project(Path.cwd())


# --- RPC methods ----------------------------------------------------------


def _rpc_register_project(app: FastAPI, params: dict) -> dict:
    root = Path(params["root"]).resolve()
    pid = app.state.db.upsert_project(root)
    return {"project_id": pid, "root": str(root)}


def _rpc_register_agent(app: FastAPI, params: dict) -> dict:
    db = app.state.db
    project_id = params.get("project_id") or _default_project(db)
    agent_id = db.insert_agent(
        project_id=project_id,
        kind=params["kind"],
        pid=params.get("pid"),
        spawned_by=params.get("spawned_by", "handoff"),
    )
    return {"agent_id": agent_id}


def _rpc_list_agents(app: FastAPI, params: dict) -> dict:
    db = app.state.db
    rows = db.list_agents(params.get("project_id"))
    out = []
    for r in rows:
        latest = db.latest_sample_for_agent(r["id"])
        out.append(
            {
                "id": r["id"],
                "kind": r["kind"],
                "pid": r["pid"],
                "status": r["status"],
                "spawned_by": r["spawned_by"],
                "started_at": r["started_at"],
                "tokens_remaining": latest["tokens_remaining"] if latest else None,
                "requests_remaining": latest["requests_remaining"] if latest else None,
                "tokens_reset_at": latest["tokens_reset_at"] if latest else None,
                "last_sample_ts": latest["ts"] if latest else None,
            }
        )
    return {"agents": out}


def _rpc_stop_agent(app: FastAPI, params: dict) -> dict:
    app.state.db.mark_agent_stopped(params["agent_id"], params.get("status", "stopped"))
    return {"ok": True}


RPC_METHODS = {
    "register_project": _rpc_register_project,
    "register_agent": _rpc_register_agent,
    "list_agents": _rpc_list_agents,
    "stop_agent": _rpc_stop_agent,
}


def serve(host: str = "127.0.0.1", port: int = DEFAULT_INGEST_PORT) -> None:
    import uvicorn

    uvicorn.run(build_app(), host=host, port=port, log_level="info")


if __name__ == "__main__":  # pragma: no cover
    serve()
