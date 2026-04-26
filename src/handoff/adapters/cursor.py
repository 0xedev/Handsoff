from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Iterable, Optional

from handoff.adapters.base import Adapter, ProcessMatch
from handoff.storage.db import RateSample


class CursorAdapter(Adapter):
    """Cursor + Google Antigravity. IDE-embedded; best-effort detection only.

    Both run as Electron processes with TLS pinning, so the proxy will likely
    fail to MITM their model traffic. We mark them 'unmetered' in v0.1 and rely
    on a companion VSCode/Cursor extension in v0.3 for usage data.
    """

    kind = "cursor"
    binaries = ("cursor", "antigravity", "Cursor", "Antigravity")
    api_hosts = ()  # proxy classification doesn't apply

    def detect(self, procs: Iterable[dict]) -> list[ProcessMatch]:
        out: list[ProcessMatch] = []
        targets = {b.lower() for b in self.binaries}
        for p in procs:
            cmd = tuple(p.get("cmdline") or ())
            name = (p.get("name") or "").lower()
            if not cmd:
                continue
            head = cmd[0].rsplit("/", 1)[-1].lower()
            if head in targets or name in targets:
                # Skip Electron child processes (renderer, gpu, utility)
                if any(arg.startswith("--type=") for arg in cmd):
                    continue
                out.append(ProcessMatch(pid=int(p["pid"]), name=name or head, cmdline=cmd))
        return out

    def context_files(self, project_root: Path) -> list[Path]:
        return [project_root / ".cursorrules"]

    def parse_headers(self, headers: dict[str, str]) -> Optional[RateSample]:
        return None  # see class docstring

    def spawn(self, project_root, env=None, extra_args=None) -> subprocess.Popen:
        raise NotImplementedError(
            "Cursor / Antigravity cannot be reliably spawned from CLI; use `handoff attach`."
        )
