"""Failover policy: subscribes to RateLimitUpdated and decides when to hand off.

Per-project config lives in `<project>/.handoff/config.toml`:

    [failover]
    tokens_remaining_pct = 10.0     # trigger when tokens drop below this %
    tokens_remaining_abs = 1000     # OR when below this absolute count
    requests_remaining = 5          # OR when fewer than this many requests left
    chain = ["claude", "codex", "copilot"]
    auto_spawn = true
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

try:
    import tomllib  # py311+
except ImportError:  # pragma: no cover
    import tomli as tomllib  # type: ignore

from handoff.adapters import get_adapter
from handoff.context.engine import ContextEngine
from handoff.daemon.spawn import headless_spawn
from handoff.storage.db import Database

log = logging.getLogger("handoff.failover")


@dataclass
class FailoverConfig:
    tokens_remaining_pct: float = 10.0
    tokens_remaining_abs: Optional[int] = None
    requests_remaining: int = 5
    chain: tuple[str, ...] = ("claude", "codex", "copilot")
    auto_spawn: bool = True


def load_config(project_root: Path) -> FailoverConfig:
    cfg_path = project_root / ".handoff" / "config.toml"
    if not cfg_path.exists():
        return FailoverConfig()
    try:
        data = tomllib.loads(cfg_path.read_text())
    except Exception as e:  # noqa: BLE001
        log.warning("failed to parse %s: %s", cfg_path, e)
        return FailoverConfig()
    f = data.get("failover", {})
    chain = tuple(f.get("chain", FailoverConfig.chain))
    return FailoverConfig(
        tokens_remaining_pct=float(f.get("tokens_remaining_pct", 10.0)),
        tokens_remaining_abs=f.get("tokens_remaining_abs"),
        requests_remaining=int(f.get("requests_remaining", 5)),
        chain=chain,
        auto_spawn=bool(f.get("auto_spawn", True)),
    )


def should_trigger(
    cfg: FailoverConfig,
    *,
    tokens_remaining: Optional[int],
    requests_remaining: Optional[int],
    initial_tokens: Optional[int] = None,
) -> tuple[bool, str]:
    """Return (trigger, reason)."""
    if tokens_remaining is not None and cfg.tokens_remaining_abs is not None:
        if tokens_remaining < cfg.tokens_remaining_abs:
            return True, f"tokens_remaining={tokens_remaining} < abs={cfg.tokens_remaining_abs}"
    if (
        tokens_remaining is not None
        and initial_tokens is not None
        and initial_tokens > 0
    ):
        pct = 100.0 * tokens_remaining / initial_tokens
        if pct < cfg.tokens_remaining_pct:
            return True, f"tokens_remaining={tokens_remaining} ({pct:.1f}% < {cfg.tokens_remaining_pct}%)"
    if requests_remaining is not None and requests_remaining < cfg.requests_remaining:
        return True, f"requests_remaining={requests_remaining} < {cfg.requests_remaining}"
    return False, ""


class FailoverPolicy:
    """Subscribes to RateLimitUpdated; spawns next agent in chain on trigger.

    De-duplication: once a (from_agent_id, to_kind) handoff fires, that pair
    is not retried until the failing agent's row is replaced (i.e. ended_at
    is set).
    """

    def __init__(self, db: Database):
        self.db = db
        self._fired: set[tuple[int, str]] = set()
        self._initial_tokens: dict[int, int] = {}

    def on_rate_limit(self, payload: dict[str, Any]) -> None:
        agent_id = payload.get("agent_id")
        if agent_id is None:
            return
        tokens = payload.get("tokens_remaining")
        requests = payload.get("requests_remaining")

        # Track the highest tokens_remaining we've ever seen for this agent
        # as a stand-in for the initial budget (no provider gives us limit).
        if tokens is not None:
            prev = self._initial_tokens.get(agent_id, 0)
            if tokens > prev:
                self._initial_tokens[agent_id] = tokens

        project_id = self.db.project_id_for_agent(agent_id)
        if project_id is None:
            return
        root_str = self.db.project_root(project_id)
        if not root_str:
            return
        root = Path(root_str)
        cfg = load_config(root)

        trigger, reason = should_trigger(
            cfg,
            tokens_remaining=tokens,
            requests_remaining=requests,
            initial_tokens=self._initial_tokens.get(agent_id),
        )
        if not trigger:
            return

        next_kind = self._pick_next(cfg.chain, payload.get("kind"))
        if next_kind is None:
            log.info("no failover candidate for kind=%s in chain", payload.get("kind"))
            return

        key = (agent_id, next_kind)
        if key in self._fired:
            return
        self._fired.add(key)

        log.warning(
            "failover triggered: agent_id=%s kind=%s reason=%s -> %s",
            agent_id, payload.get("kind"), reason, next_kind,
        )
        self.execute(
            from_agent_id=agent_id,
            to_kind=next_kind,
            project_root=root,
            project_id=project_id,
            reason=reason,
            auto_spawn=cfg.auto_spawn,
        )

    def execute(
        self,
        *,
        from_agent_id: Optional[int],
        to_kind: str,
        project_root: Path,
        project_id: int,
        reason: str,
        auto_spawn: bool = True,
    ) -> dict:
        engine = ContextEngine(project_root)
        snapshot = engine.snapshot(
            note=f"Failover from agent_id={from_agent_id}: {reason}"
        )

        new_agent_id: Optional[int] = None
        spawn_pid: Optional[int] = None
        if auto_spawn:
            try:
                proc = headless_spawn(
                    to_kind,
                    project_root,
                    prompt=f"Resuming work. Read this snapshot first: {snapshot}",
                )
                if proc is not None:
                    spawn_pid = proc.pid
                    new_agent_id = self.db.insert_agent(
                        project_id=project_id,
                        kind=to_kind,
                        pid=spawn_pid,
                        spawned_by="handoff",
                    )
            except Exception as e:  # noqa: BLE001
                log.exception("failover spawn failed: %s", e)

        handoff_id = self.db.insert_handoff(
            from_agent_id=from_agent_id,
            to_agent_id=new_agent_id,
            reason=reason,
            snapshot_path=str(snapshot),
        )
        return {
            "handoff_id": handoff_id,
            "to_agent_id": new_agent_id,
            "to_pid": spawn_pid,
            "snapshot_path": str(snapshot),
        }

    @staticmethod
    def _pick_next(chain: tuple[str, ...], current: Optional[str]) -> Optional[str]:
        for k in chain:
            if k != current:
                return k
        return None
