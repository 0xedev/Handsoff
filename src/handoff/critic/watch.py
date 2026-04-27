"""File watcher used by `handoff critic watch`.

No external deps: polls mtimes every `interval` seconds. Fires the callback
once per debounce window so a flurry of saves only triggers one critic run.
"""

from __future__ import annotations

import logging
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable, Optional

log = logging.getLogger("handoff.watch")


def list_tracked_files(root: Path) -> list[Path]:
    """Prefer `git ls-files` (respects .gitignore). Fall back to a basic walk."""
    try:
        res = subprocess.run(
            ["git", "ls-files"],
            cwd=str(root),
            capture_output=True,
            text=True,
            check=False,
            timeout=5,
        )
        if res.returncode == 0 and res.stdout:
            return [root / line for line in res.stdout.splitlines() if line]
    except (FileNotFoundError, subprocess.SubprocessError):
        pass
    out: list[Path] = []
    skip_dirs = {".git", ".handoff", "node_modules", "__pycache__", ".venv", "venv"}
    for p in root.rglob("*"):
        if not p.is_file():
            continue
        if any(part in skip_dirs for part in p.parts):
            continue
        out.append(p)
    return out


@dataclass
class WatchTick:
    changed: set[Path]
    fired: bool


class WatchLoop:
    """Polling watcher that fires `callback` after a debounce window."""

    def __init__(
        self,
        root: Path,
        callback: Callable[[set[Path]], None],
        *,
        interval: float = 2.0,
        debounce: float = 3.0,
        extra_paths: Optional[Iterable[Path]] = None,
    ):
        self.root = root.resolve()
        self.callback = callback
        self.interval = interval
        self.debounce = debounce
        self.extra_paths = [Path(p) for p in (extra_paths or [])]
        self._mtimes: dict[Path, float] = {}
        self._pending: set[Path] = set()
        self._last_change_ts: Optional[float] = None
        self._snapshot_files()

    def _snapshot_files(self) -> None:
        for p in self._scan():
            try:
                self._mtimes[p] = p.stat().st_mtime
            except OSError:
                continue

    def _scan(self) -> list[Path]:
        files = list_tracked_files(self.root)
        files.extend(p for p in self.extra_paths if p.exists())
        return files

    def tick(self, *, now: Optional[float] = None) -> WatchTick:
        """One poll iteration. Returns the changed set + whether the
        callback fired this tick."""
        t = now if now is not None else time.time()
        changed: set[Path] = set()
        for p in self._scan():
            try:
                m = p.stat().st_mtime
            except OSError:
                continue
            prev = self._mtimes.get(p)
            if prev is None or m > prev:
                self._mtimes[p] = m
                if prev is not None:
                    changed.add(p)
        if changed:
            self._pending |= changed
            self._last_change_ts = t

        fired = False
        if (
            self._pending
            and self._last_change_ts is not None
            and (t - self._last_change_ts) >= self.debounce
        ):
            try:
                self.callback(set(self._pending))
            finally:
                fired = True
                self._pending.clear()
                self._last_change_ts = None
        return WatchTick(changed=changed, fired=fired)

    def run_forever(self) -> None:  # pragma: no cover - blocking loop
        log.info("watching %s (interval=%.1fs debounce=%.1fs)", self.root, self.interval, self.debounce)
        try:
            while True:
                self.tick()
                time.sleep(self.interval)
        except KeyboardInterrupt:
            log.info("watch loop stopped")
