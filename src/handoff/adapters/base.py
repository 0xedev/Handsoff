from __future__ import annotations

import os
import shutil
import subprocess
from abc import ABC, abstractmethod
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Optional

from handoff.storage.db import RateSample


@dataclass(frozen=True)
class ProcessMatch:
    pid: int
    name: str
    cmdline: tuple[str, ...]


class Adapter(ABC):
    """Per-agent integration: detect, inject context, parse usage, spawn."""

    kind: str = ""
    binaries: tuple[str, ...] = ()
    api_hosts: tuple[str, ...] = ()

    def detect(self, procs: Iterable[dict]) -> list[ProcessMatch]:
        """Return processes from the iterable that look like this agent.

        Each `proc` is a dict with keys 'pid', 'name', 'cmdline' (list).
        Default implementation matches against `self.binaries`.
        """
        out: list[ProcessMatch] = []
        for p in procs:
            name = (p.get("name") or "").lower()
            cmd = tuple(p.get("cmdline") or ())
            if not cmd:
                continue
            head = Path(cmd[0]).name.lower()
            if name in {b.lower() for b in self.binaries} or head in {
                b.lower() for b in self.binaries
            }:
                out.append(ProcessMatch(pid=int(p["pid"]), name=name, cmdline=cmd))
        return out

    def classify_host(self, host: str) -> bool:
        host = host.lower()
        return any(host == h or host.endswith("." + h) for h in self.api_hosts)

    @abstractmethod
    def context_files(self, project_root: Path) -> list[Path]:
        """Files this agent reads as project context (relative to project root or absolute)."""

    @abstractmethod
    def parse_headers(self, headers: dict[str, str]) -> Optional[RateSample]:
        """Convert a response's headers into a RateSample, or None if not applicable."""

    def spawn(
        self,
        project_root: Path,
        env: Optional[dict[str, str]] = None,
        extra_args: Optional[list[str]] = None,
    ) -> subprocess.Popen:
        """Default spawn: run the first available binary with proxy env wired."""
        bin_name = self._resolve_binary()
        if bin_name is None:
            raise FileNotFoundError(
                f"No {self.kind} binary found on PATH (looked for: {', '.join(self.binaries)})"
            )
        full_env = os.environ.copy()
        if env:
            full_env.update(env)
        argv = [bin_name, *(extra_args or [])]
        return subprocess.Popen(argv, cwd=str(project_root), env=full_env)

    def _resolve_binary(self) -> Optional[str]:
        for b in self.binaries:
            path = shutil.which(b)
            if path:
                return path
        return None


def _parse_int(headers: dict[str, str], key: str) -> Optional[int]:
    v = headers.get(key) or headers.get(key.lower())
    if v is None:
        return None
    try:
        return int(v)
    except (ValueError, TypeError):
        return None


def _parse_reset_epoch(headers: dict[str, str], key: str) -> Optional[int]:
    """Parse a reset header. Anthropic gives ISO-8601; OpenAI sometimes gives '2s' or epoch."""
    import time
    from datetime import datetime, timezone

    v = headers.get(key) or headers.get(key.lower())
    if v is None:
        return None
    v = v.strip()
    # ISO-8601
    if "T" in v:
        try:
            dt = datetime.fromisoformat(v.replace("Z", "+00:00"))
            return int(dt.astimezone(timezone.utc).timestamp())
        except ValueError:
            pass
    # Duration string like "5s", "2m30s"
    if v[-1:] in {"s", "m", "h"}:
        seconds = 0
        num = ""
        for ch in v:
            if ch.isdigit() or ch == ".":
                num += ch
            elif ch == "s" and num:
                seconds += int(float(num)); num = ""
            elif ch == "m" and num:
                seconds += int(float(num)) * 60; num = ""
            elif ch == "h" and num:
                seconds += int(float(num)) * 3600; num = ""
        if seconds:
            return int(time.time()) + seconds
    # Bare integer (epoch or seconds-from-now)
    try:
        n = int(float(v))
        return n if n > 1_000_000_000 else int(time.time()) + n
    except ValueError:
        return None
