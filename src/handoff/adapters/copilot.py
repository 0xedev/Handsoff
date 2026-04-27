from __future__ import annotations

from pathlib import Path
from typing import Iterable, Optional

from handoff.adapters.base import Adapter, ProcessMatch
from handoff.storage.db import RateSample


class CopilotAdapter(Adapter):
    """GitHub Copilot CLI. Usage is opaque; we only count requests + 429s in v0.2."""

    kind = "copilot"
    binaries = ("gh", "copilot")
    api_hosts = ("api.githubcopilot.com", "api.github.com")

    def detect(self, procs: Iterable[dict]) -> list[ProcessMatch]:
        # `gh` alone is too broad; require `gh copilot ...` in cmdline.
        out: list[ProcessMatch] = []
        for p in procs:
            cmd = tuple(p.get("cmdline") or ())
            if not cmd:
                continue
            head = cmd[0].rsplit("/", 1)[-1].lower()
            if head == "copilot":
                out.append(ProcessMatch(pid=int(p["pid"]), name="copilot", cmdline=cmd))
            elif head == "gh" and len(cmd) > 1 and cmd[1].lower() == "copilot":
                out.append(ProcessMatch(pid=int(p["pid"]), name="gh-copilot", cmdline=cmd))
        return out

    def context_files(self, project_root: Path) -> list[Path]:
        return [project_root / ".github" / "copilot-instructions.md"]

    def parse_headers(self, headers: dict[str, str]) -> Optional[RateSample]:
        # GitHub doesn't expose per-request token budget for Copilot. Fall back to
        # request-counting only; record nothing here, the daemon counts 200/429s.
        return None
