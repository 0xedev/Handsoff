"""Critic loop: cheap worker model produces a plan + diff, expensive critic
reviews it. Both clients route through the local proxy so usage shows up in
`handoff agents`.

v0.2 scope (one-shot):
    1. Worker (Haiku) reads brain.md + task -> emits a plan and a unified diff.
    2. Critic (Opus) reviews -> {verdict, notes}.
    3. If verdict='redirect', worker gets one revision pass with critic's notes.
    4. Result written to .handoff/scratch/critic-<ts>.{md,diff}; nothing applied.

The user reviews the diff and applies it themselves (e.g. `git apply`). v0.3
will add execution tools.
"""

from __future__ import annotations

import json
import logging
import os
import re
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

log = logging.getLogger("handoff.critic")

DEFAULT_WORKER_MODEL = "claude-haiku-4-5-20251001"
DEFAULT_CRITIC_MODEL = "claude-opus-4-7"

WORKER_SYSTEM = """\
You are the Worker in a two-model handoff loop.

Your job: given a task and the project's brain.md, produce:
  1. A short numbered plan (3-7 steps).
  2. A unified diff (git-style, with `--- a/path` and `+++ b/path` lines)
     that implements the FIRST useful step. Keep it small; the Critic will
     review and either approve or redirect.

If the task is exploratory (no edits required), return an empty diff and
say so in the plan.

Output format - exactly this, no extra prose:

<plan>
1. ...
2. ...
</plan>

<diff>
diff --git a/path b/path
@@
- old
+ new
</diff>
"""

CRITIC_SYSTEM = """\
You are the Critic in a two-model handoff loop.

Review the Worker's plan + diff. Decide:
  - approve: the diff is correct, safe, and implements the first step well.
  - redirect: the diff is wrong/risky/off-task. Provide concrete notes for
    the Worker's revision.

Output ONLY a single JSON object on one line:
  {"verdict": "approve" | "redirect", "notes": "<short reasoning>"}
"""

PLAN_RE = re.compile(r"<plan>(.*?)</plan>", re.DOTALL)
DIFF_RE = re.compile(r"<diff>(.*?)</diff>", re.DOTALL)


@dataclass
class CriticResult:
    verdict: str
    plan: str
    diff: str
    notes: str
    worker_tokens: int = 0
    critic_tokens: int = 0
    artifacts: list[Path] = field(default_factory=list)


def _extract(tag_re: re.Pattern, text: str) -> str:
    m = tag_re.search(text)
    return (m.group(1) if m else "").strip()


class CriticRunner:
    def __init__(
        self,
        project_root: Path,
        *,
        worker_model: str = DEFAULT_WORKER_MODEL,
        critic_model: str = DEFAULT_CRITIC_MODEL,
        proxy_url: Optional[str] = "http://127.0.0.1:8080",
    ):
        self.root = project_root.resolve()
        self.worker_model = worker_model
        self.critic_model = critic_model
        self.proxy_url = proxy_url

    def _client(self):
        # Lazy import: anthropic SDK is heavy and only needed when running.
        import anthropic  # type: ignore
        import httpx

        if self.proxy_url:
            http = httpx.Client(
                proxy=self.proxy_url,
                timeout=httpx.Timeout(60.0, connect=10.0),
            )
            return anthropic.Anthropic(http_client=http)
        return anthropic.Anthropic()

    def _ask(self, client, model: str, system: str, user: str) -> tuple[str, int]:
        msg = client.messages.create(
            model=model,
            max_tokens=2048,
            system=system,
            messages=[{"role": "user", "content": user}],
        )
        text = "".join(b.text for b in msg.content if getattr(b, "type", "") == "text")
        used = (msg.usage.input_tokens or 0) + (msg.usage.output_tokens or 0)
        return text, used

    def run(self, task: str, *, brain: Optional[str] = None) -> CriticResult:
        if brain is None:
            brain_path = self.root / ".handoff" / "brain.md"
            brain = brain_path.read_text() if brain_path.exists() else ""

        client = self._client()

        worker_prompt = (
            f"## Project brain\n\n{brain}\n\n"
            f"## Task\n\n{task}\n\n"
            "Now produce <plan> and <diff>."
        )
        worker_text, worker_tokens = self._ask(
            client, self.worker_model, WORKER_SYSTEM, worker_prompt
        )
        plan = _extract(PLAN_RE, worker_text)
        diff = _extract(DIFF_RE, worker_text)

        critic_prompt = (
            f"## Task\n{task}\n\n"
            f"## Worker's plan\n{plan}\n\n"
            f"## Worker's diff\n```diff\n{diff}\n```\n\n"
            "Output the JSON verdict now."
        )
        critic_text, critic_tokens = self._ask(
            client, self.critic_model, CRITIC_SYSTEM, critic_prompt
        )
        verdict, notes = self._parse_verdict(critic_text)

        # One revision pass on redirect.
        if verdict == "redirect":
            revise_prompt = (
                f"{worker_prompt}\n\n"
                f"## Critic feedback (your previous attempt was rejected)\n{notes}\n\n"
                "Revise. Produce <plan> and <diff> again."
            )
            worker_text2, w2 = self._ask(
                client, self.worker_model, WORKER_SYSTEM, revise_prompt
            )
            worker_tokens += w2
            plan = _extract(PLAN_RE, worker_text2) or plan
            diff = _extract(DIFF_RE, worker_text2) or diff

            critic_prompt2 = (
                f"## Task\n{task}\n\n"
                f"## Revised plan\n{plan}\n\n"
                f"## Revised diff\n```diff\n{diff}\n```\n\n"
                "Output the JSON verdict now."
            )
            critic_text2, c2 = self._ask(
                client, self.critic_model, CRITIC_SYSTEM, critic_prompt2
            )
            critic_tokens += c2
            verdict, notes = self._parse_verdict(critic_text2)

        artifacts = self._write_artifacts(task, plan, diff, verdict, notes)
        return CriticResult(
            verdict=verdict,
            plan=plan,
            diff=diff,
            notes=notes,
            worker_tokens=worker_tokens,
            critic_tokens=critic_tokens,
            artifacts=artifacts,
        )

    def _parse_verdict(self, text: str) -> tuple[str, str]:
        # Find the first JSON object on any line.
        for line in text.splitlines():
            line = line.strip()
            if not (line.startswith("{") and line.endswith("}")):
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError:
                continue
            v = str(d.get("verdict", "")).lower().strip()
            if v in {"approve", "redirect"}:
                return v, str(d.get("notes", "")).strip()
        return "redirect", "critic returned malformed verdict; defaulting to redirect"

    def _write_artifacts(
        self,
        task: str,
        plan: str,
        diff: str,
        verdict: str,
        notes: str,
    ) -> list[Path]:
        ts = int(time.time())
        scratch = self.root / ".handoff" / "scratch"
        scratch.mkdir(parents=True, exist_ok=True)
        md_path = scratch / f"critic-{ts}.md"
        md_path.write_text(
            f"# Critic run {ts}\n\n"
            f"## Task\n\n{task}\n\n"
            f"## Verdict: {verdict}\n\n{notes}\n\n"
            f"## Plan\n\n{plan}\n\n"
            f"## Diff\n\n```diff\n{diff}\n```\n"
        )
        out: list[Path] = [md_path]
        if diff.strip():
            diff_path = scratch / f"critic-{ts}.diff"
            diff_path.write_text(diff if diff.endswith("\n") else diff + "\n")
            out.append(diff_path)
        return out
