"""CriticRunner tests using a stub anthropic client.

We don't hit the real API — we patch `_ask` to return canned strings for
the worker and critic, then verify the parsing, revision, and artifact
writing logic.
"""

from pathlib import Path

import pytest

from handoff.context.engine import init_project
from handoff.critic.runner import CriticRunner


def _patch_ask(runner: CriticRunner, responses: list[tuple[str, int]]):
    """Make runner._ask return the next item in `responses` per call."""
    iter_resp = iter(responses)

    def fake_ask(_client, _model, _system, _user):
        return next(iter_resp)

    runner._ask = fake_ask  # type: ignore[method-assign]
    runner._client = lambda: object()  # type: ignore[method-assign]


def test_approve_path_writes_md_and_diff(tmp_path: Path):
    init_project(tmp_path)
    runner = CriticRunner(tmp_path, proxy_url=None)
    worker_out = (
        "<plan>\n1. add greet()\n</plan>\n"
        "<diff>\ndiff --git a/x.py b/x.py\n@@\n+def greet(): pass\n</diff>"
    )
    critic_out = '{"verdict": "approve", "notes": "looks good"}'
    _patch_ask(runner, [(worker_out, 100), (critic_out, 50)])

    res = runner.run("add a greet function")
    assert res.verdict == "approve"
    assert "greet" in res.diff
    assert res.worker_tokens == 100
    assert res.critic_tokens == 50
    names = {a.name for a in res.artifacts}
    assert any(n.endswith(".md") for n in names)
    assert any(n.endswith(".diff") for n in names)


def test_redirect_triggers_one_revision(tmp_path: Path):
    init_project(tmp_path)
    runner = CriticRunner(tmp_path, proxy_url=None)

    first_worker = "<plan>\n1. wrong\n</plan>\n<diff>\nbad\n</diff>"
    redirect = '{"verdict": "redirect", "notes": "wrong file"}'
    second_worker = "<plan>\n1. fixed\n</plan>\n<diff>\nbetter\n</diff>"
    approve = '{"verdict": "approve", "notes": "ok"}'
    _patch_ask(
        runner,
        [(first_worker, 10), (redirect, 5), (second_worker, 12), (approve, 6)],
    )

    res = runner.run("do something")
    assert res.verdict == "approve"
    assert "fixed" in res.plan
    assert "better" in res.diff
    assert res.worker_tokens == 22  # 10 + 12
    assert res.critic_tokens == 11  # 5 + 6


def test_malformed_critic_defaults_to_redirect(tmp_path: Path):
    init_project(tmp_path)
    runner = CriticRunner(tmp_path, proxy_url=None)
    worker_out = "<plan>1. x</plan><diff>diff</diff>"
    bad_critic = "I think it's fine, no JSON for you"
    # On redirect, runner attempts a revision; supply more responses.
    _patch_ask(
        runner,
        [(worker_out, 1), (bad_critic, 1), (worker_out, 1), (bad_critic, 1)],
    )
    res = runner.run("anything")
    assert res.verdict == "redirect"
    assert "malformed" in res.notes or "redirect" in res.verdict


def test_empty_diff_does_not_write_diff_file(tmp_path: Path):
    init_project(tmp_path)
    runner = CriticRunner(tmp_path, proxy_url=None)
    worker_out = "<plan>\n1. exploration only\n</plan>\n<diff>\n</diff>"
    approve = '{"verdict": "approve", "notes": "nothing to do"}'
    _patch_ask(runner, [(worker_out, 1), (approve, 1)])
    res = runner.run("explore")
    names = {a.name for a in res.artifacts}
    assert any(n.endswith(".md") for n in names)
    assert not any(n.endswith(".diff") for n in names)
