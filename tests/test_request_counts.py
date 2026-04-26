from pathlib import Path

from handoff.storage.db import Database


def test_bump_request_count_aggregates(tmp_path: Path):
    db = Database(tmp_path / "t.db")
    pid = db.upsert_project(tmp_path)
    aid = db.insert_agent(project_id=pid, kind="copilot", pid=1, spawned_by="user")

    db.bump_request_count(aid, 200)
    db.bump_request_count(aid, 200)
    db.bump_request_count(aid, 429)

    row = db.request_count_for_agent(aid)
    assert row is not None
    assert row["total"] == 3
    assert row["rate_limited"] == 1
    assert row["last_429_at"] is not None
    assert row["last_request_at"] >= row["last_429_at"]
    db.close()


def test_request_count_none_for_unknown(tmp_path: Path):
    db = Database(tmp_path / "t.db")
    assert db.request_count_for_agent(999) is None
    db.close()
