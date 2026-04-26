"""ContextEngine: project brain + per-agent derived context files.

v0.1 strategy: copy `brain.md` literally into each adapter's context files,
stripping any leading YAML frontmatter. Snapshot writes a timestamped scratch
note used by failover (no critic-summarization yet — that's v0.2).
"""

from __future__ import annotations

import re
import time
from pathlib import Path
from typing import Optional

from handoff.adapters import ALL_ADAPTERS

DEFAULT_BRAIN = """\
# Project brain

This is the canonical context for every AI agent working in this project.
Edit it directly. `handoff sync` will mirror it into each agent's native
context file (CLAUDE.md, AGENTS.md, .cursorrules, copilot-instructions.md).

## Goals
-

## Architecture
-

## Conventions
-

## Open questions
-
"""

DEFAULT_CONFIG = """\
# .handoff/config.toml
# Per-project overrides for thresholds, models, and failover chains.

[failover]
# trigger handoff when remaining tokens drop below 10% of an undefined budget,
# OR when remaining requests drop below this many.
tokens_remaining_pct = 10.0
requests_remaining = 5

# Order to try when the active agent runs out.
chain = ["claude", "codex", "copilot"]

[critic]
worker_model = "claude-haiku-4-5-20251001"
critic_model = "claude-opus-4-7"
"""

FRONTMATTER_RE = re.compile(r"^---\n.*?\n---\n", re.DOTALL)


def _strip_frontmatter(text: str) -> str:
    return FRONTMATTER_RE.sub("", text, count=1)


def init_project(root: Path) -> Path:
    """Create .handoff/ scaffolding in the project root. Idempotent."""
    h = root / ".handoff"
    (h / "decisions").mkdir(parents=True, exist_ok=True)
    (h / "scratch").mkdir(parents=True, exist_ok=True)
    (h / "derived").mkdir(parents=True, exist_ok=True)
    brain = h / "brain.md"
    if not brain.exists():
        brain.write_text(DEFAULT_BRAIN)
    config = h / "config.toml"
    if not config.exists():
        config.write_text(DEFAULT_CONFIG)
    gitignore = h / ".gitignore"
    if not gitignore.exists():
        gitignore.write_text("derived/\nscratch/\n")
    return h


class ContextEngine:
    def __init__(self, project_root: Path):
        self.root = project_root.resolve()
        self.handoff_dir = self.root / ".handoff"
        self.brain_path = self.handoff_dir / "brain.md"
        self.derived_dir = self.handoff_dir / "derived"

    def sync(self) -> list[Path]:
        """Render brain.md into each adapter's context files. Returns written paths."""
        if not self.brain_path.exists():
            raise FileNotFoundError(f"no brain at {self.brain_path}; run `handoff init`")
        body = _strip_frontmatter(self.brain_path.read_text())
        written: list[Path] = []
        for adapter in ALL_ADAPTERS:
            for target in adapter.context_files(self.root):
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(body)
                written.append(target)
        return written

    def snapshot(self, note: Optional[str] = None) -> Path:
        """Write a timestamped scratch handoff note. Used by failover."""
        ts = int(time.time())
        path = self.handoff_dir / "scratch" / f"handoff-{ts}.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        body = (
            f"# Handoff snapshot {ts}\n\n"
            f"Automated snapshot for agent handoff.\n\n"
            f"## Project brain (verbatim)\n\n"
            + _strip_frontmatter(self.brain_path.read_text() if self.brain_path.exists() else "")
        )
        if note:
            body += f"\n\n## Note\n\n{note}\n"
        path.write_text(body)
        return path
