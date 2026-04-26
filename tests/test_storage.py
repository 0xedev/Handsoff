from pathlib import Path

from handoff.storage.db import Database, RateSample


def test_full_roundtrip(tmp_path: Path):
    db = Database(tmp_path / "test.db")
    pid = db.upsert_project(tmp_path)
    assert pid > 0
    # idempotent
    pid2 = db.upsert_project(tmp_path)
    assert pid == pid2

    agent_id = db.insert_agent(project_id=pid, kind="claude", pid=1234, spawned_by="user")
    assert agent_id > 0

    sample = RateSample(
        provider="anthropic",
        tokens_remaining=100,
        requests_remaining=50,
        tokens_reset_at=1_700_000_000,
        requests_reset_at=1_700_000_000,
        raw_headers={"anthropic-ratelimit-tokens-remaining": "100"},
    )
    sid = db.insert_rate_sample(agent_id, sample)
    assert sid > 0

    latest = db.latest_sample_for_agent(agent_id)
    assert latest is not None
    assert latest["tokens_remaining"] == 100
    assert latest["provider"] == "anthropic"

    found = db.find_agent_by_pid(1234)
    assert found is not None
    assert found["id"] == agent_id

    rows = db.list_agents(pid)
    assert len(rows) == 1

    db.mark_agent_stopped(agent_id)
    assert db.list_agents(pid) == []
    db.close()
