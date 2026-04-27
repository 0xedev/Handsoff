from __future__ import annotations

from pathlib import Path
from typing import Optional

from handoff.adapters.base import Adapter, _parse_int, _parse_reset_epoch
from handoff.storage.db import RateSample


class CodexAdapter(Adapter):
    kind = "codex"
    binaries = ("codex",)
    api_hosts = ("api.openai.com",)

    def context_files(self, project_root: Path) -> list[Path]:
        return [project_root / "AGENTS.md"]

    def parse_headers(self, headers: dict[str, str]) -> Optional[RateSample]:
        h = {k.lower(): v for k, v in headers.items()}
        if not any(k.startswith("x-ratelimit-") for k in h):
            return None
        return RateSample(
            provider="openai",
            tokens_remaining=_parse_int(h, "x-ratelimit-remaining-tokens"),
            requests_remaining=_parse_int(h, "x-ratelimit-remaining-requests"),
            tokens_reset_at=_parse_reset_epoch(h, "x-ratelimit-reset-tokens"),
            requests_reset_at=_parse_reset_epoch(h, "x-ratelimit-reset-requests"),
            raw_headers={k: v for k, v in h.items() if k.startswith("x-ratelimit-")},
        )
