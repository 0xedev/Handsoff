from pathlib import Path

from handoff.context.engine import init_project
from handoff.critic.runner import CriticRunner


def _patch_ask(runner: CriticRunner, responses: list[tuple[str, int]]) -> None:
    iter_resp = iter(responses)
    runner._ask = lambda *_args, **_kw: next(iter_resp)  # type: ignore[method-assign]
    runner._client = lambda: object()  # type: ignore[method-assign]


def test_summarize_uses_critic_model_and_returns_brief(tmp_path: Path):
    init_project(tmp_path)
    (tmp_path / ".handoff" / "brain.md").write_text("# brain\n\nbuild a thing\n")
    runner = CriticRunner(tmp_path, proxy_url=None)
    _patch_ask(
        runner,
        [
            (
                "## Where we left off\n"
                "wired the daemon\n\n"
                "## What's blocked / next step\n"
                "wire the proxy\n\n"
                "## Anything the new agent must NOT change\n"
                "schema",
                250,
            )
        ],
    )

    brief, used = runner.summarize_for_handoff(reason="rate-limit failover")
    assert "Where we left off" in brief
    assert used == 250


def test_summarize_includes_recent_scratch(tmp_path: Path):
    init_project(tmp_path)
    scratch = tmp_path / ".handoff" / "scratch"
    scratch.mkdir(parents=True, exist_ok=True)
    (scratch / "critic-1.md").write_text("first run notes")
    (scratch / "critic-2.md").write_text("second run notes")

    runner = CriticRunner(tmp_path, proxy_url=None)
    captured: dict[str, str] = {}

    def capture(_client, _model, _system, user):
        captured["user"] = user
        return ("brief text", 10)

    runner._ask = capture  # type: ignore[method-assign]
    runner._client = lambda: object()  # type: ignore[method-assign]

    runner.summarize_for_handoff()
    assert "first run notes" in captured["user"] or "second run notes" in captured["user"]


def test_snapshot_includes_brief_when_provided(tmp_path: Path):
    from handoff.context.engine import ContextEngine

    init_project(tmp_path)
    (tmp_path / ".handoff" / "brain.md").write_text("# brain\n\nstuff\n")
    p = ContextEngine(tmp_path).snapshot(brief="**focused brief**\n\ndo X next")
    body = p.read_text()
    assert "Brief (critic-summarized)" in body
    assert "focused brief" in body
    assert "Project brain (verbatim, for reference)" in body


def test_snapshot_falls_back_to_verbatim_without_brief(tmp_path: Path):
    from handoff.context.engine import ContextEngine

    init_project(tmp_path)
    (tmp_path / ".handoff" / "brain.md").write_text("# brain\n\nstuff\n")
    p = ContextEngine(tmp_path).snapshot()
    body = p.read_text()
    assert "Brief (critic-summarized)" not in body
    assert "Project brain (verbatim)" in body
