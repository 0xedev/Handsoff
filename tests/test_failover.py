from pathlib import Path

from handoff.context.engine import init_project
from handoff.daemon.failover import (
    FailoverConfig,
    FailoverPolicy,
    load_config,
    should_trigger,
)
from handoff.storage.db import Database


def test_should_trigger_below_pct():
    cfg = FailoverConfig(tokens_remaining_pct=10.0)
    triggered, reason = should_trigger(
        cfg, tokens_remaining=900, requests_remaining=None, initial_tokens=10000
    )
    assert triggered
    assert "tokens_remaining=900" in reason


def test_should_trigger_above_pct():
    cfg = FailoverConfig(tokens_remaining_pct=10.0)
    triggered, _ = should_trigger(
        cfg, tokens_remaining=2000, requests_remaining=None, initial_tokens=10000
    )
    assert not triggered


def test_should_trigger_below_abs():
    cfg = FailoverConfig(tokens_remaining_abs=500)
    triggered, reason = should_trigger(
        cfg, tokens_remaining=300, requests_remaining=None, initial_tokens=None
    )
    assert triggered
    assert "abs=500" in reason


def test_should_trigger_below_request_count():
    cfg = FailoverConfig(requests_remaining=5)
    triggered, _ = should_trigger(
        cfg, tokens_remaining=None, requests_remaining=2, initial_tokens=None
    )
    assert triggered


def test_should_not_trigger_when_unknown():
    cfg = FailoverConfig()
    triggered, _ = should_trigger(
        cfg, tokens_remaining=None, requests_remaining=None, initial_tokens=None
    )
    assert not triggered


def test_load_config_defaults_when_missing(tmp_path: Path):
    cfg = load_config(tmp_path)
    assert cfg.chain == ("claude", "codex", "copilot")
    assert cfg.auto_spawn is True


def test_load_config_reads_toml(tmp_path: Path):
    init_project(tmp_path)
    (tmp_path / ".handoff" / "config.toml").write_text(
        '[failover]\n'
        'tokens_remaining_pct = 25.0\n'
        'requests_remaining = 1\n'
        'chain = ["codex", "claude"]\n'
        'auto_spawn = false\n'
    )
    cfg = load_config(tmp_path)
    assert cfg.tokens_remaining_pct == 25.0
    assert cfg.requests_remaining == 1
    assert cfg.chain == ("codex", "claude")
    assert cfg.auto_spawn is False


def test_pick_next_skips_current():
    pick = FailoverPolicy._pick_next(("claude", "codex", "copilot"), "claude")
    assert pick == "codex"


def test_pick_next_first_when_unknown_current():
    pick = FailoverPolicy._pick_next(("claude", "codex"), "antigravity")
    assert pick == "claude"


def test_policy_fires_once_per_pair(tmp_path: Path):
    """De-dupe: same (agent_id, to_kind) pair only triggers once."""
    init_project(tmp_path)
    (tmp_path / ".handoff" / "config.toml").write_text(
        '[failover]\n'
        'requests_remaining = 100\n'
        'auto_spawn = false\n'  # no spawn so we don't shell out in tests
        'chain = ["codex", "claude"]\n'
    )
    db = Database(tmp_path / "test.db")
    project_id = db.upsert_project(tmp_path)
    agent_id = db.insert_agent(
        project_id=project_id, kind="claude", pid=999, spawned_by="user"
    )
    policy = FailoverPolicy(db)

    payload = {
        "agent_id": agent_id,
        "kind": "claude",
        "tokens_remaining": None,
        "requests_remaining": 1,
    }
    policy.on_rate_limit(payload)
    policy.on_rate_limit(payload)  # second time should be a no-op

    cur = db._conn.execute(
        "SELECT COUNT(*) AS n FROM handoffs WHERE from_agent_id=?", (agent_id,)
    )
    assert cur.fetchone()["n"] == 1
    db.close()
