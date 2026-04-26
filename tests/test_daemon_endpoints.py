"""Integration-style tests for the daemon's /ingest and /rpc endpoints."""

from pathlib import Path

import pytest
from fastapi.testclient import TestClient


@pytest.fixture
def client(tmp_path: Path, monkeypatch):
    monkeypatch.setenv("HANDOFF_HOME", str(tmp_path / "home"))
    # Force a fresh import so the new HANDOFF_HOME takes effect.
    import importlib
    import handoff.paths
    import handoff.storage.db
    import handoff.daemon.server as server

    importlib.reload(handoff.paths)
    importlib.reload(handoff.storage.db)
    importlib.reload(server)

    app = server.build_app()
    return TestClient(app)


def test_health(client):
    r = client.get("/health")
    assert r.status_code == 200
    assert r.json()["ok"]


def test_ingest_auto_registers_agent_and_counts(client):
    r = client.post(
        "/ingest",
        json={
            "kind": "copilot",
            "host": "api.githubcopilot.com",
            "status_code": 200,
            "pid": 42,
            "sample": None,
        },
    )
    assert r.status_code == 200
    body = r.json()
    assert body["ok"]
    assert body["agent_id"] is not None

    # 429 should be counted as rate_limited
    r2 = client.post(
        "/ingest",
        json={
            "kind": "copilot",
            "host": "api.githubcopilot.com",
            "status_code": 429,
            "pid": 42,
        },
    )
    assert r2.json()["agent_id"] == body["agent_id"]

    # list_agents should now show counts
    res = client.post(
        "/rpc", json={"method": "list_agents", "params": {}}
    ).json()
    assert res["ok"]
    agents = res["result"]["agents"]
    me = next(a for a in agents if a["pid"] == 42)
    assert me["total_requests"] == 2
    assert me["rate_limited_count"] == 1


def test_ingest_with_sample_records_rate_sample(client):
    client.post(
        "/ingest",
        json={
            "kind": "claude",
            "host": "api.anthropic.com",
            "status_code": 200,
            "pid": 100,
            "sample": {
                "provider": "anthropic",
                "tokens_remaining": 50000,
                "requests_remaining": 100,
                "tokens_reset_at": 1700000000,
                "requests_reset_at": 1700000000,
                "raw_headers": {},
            },
        },
    )
    res = client.post("/rpc", json={"method": "list_agents", "params": {}}).json()
    me = next(a for a in res["result"]["agents"] if a["pid"] == 100)
    assert me["tokens_remaining"] == 50000
    assert me["requests_remaining"] == 100


def test_attach_agent_rpc(client):
    r = client.post(
        "/rpc",
        json={"method": "attach_agent", "params": {"kind": "claude", "pid": 555}},
    )
    body = r.json()
    assert body["ok"]
    assert body["result"]["agent_id"] > 0


def test_unknown_rpc_returns_error(client):
    r = client.post("/rpc", json={"method": "nope", "params": {}})
    assert r.status_code == 200
    assert r.json()["ok"] is False
    assert "unknown" in r.json()["error"]
