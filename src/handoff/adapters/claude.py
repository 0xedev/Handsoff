from __future__ import annotations

from pathlib import Path
from typing import Optional

from handoff.adapters.base import Adapter, _parse_int, _parse_reset_epoch
from handoff.storage.db import RateSample


class ClaudeAdapter(Adapter):
    kind = "claude"
    binaries = ("claude", "claude-code")
    api_hosts = ("api.anthropic.com",)

    def context_files(self, project_root: Path) -> list[Path]:
        return [project_root / "CLAUDE.md"]

    def parse_headers(self, headers: dict[str, str]) -> Optional[RateSample]:
        # Normalize keys to lowercase once
        h = {k.lower(): v for k, v in headers.items()}
        # Look for any anthropic-ratelimit-* header to confirm this is from Anthropic
        if not any(k.startswith("anthropic-ratelimit-") for k in h):
            return None
        return RateSample(
            provider="anthropic",
            tokens_remaining=_parse_int(h, "anthropic-ratelimit-tokens-remaining"),
            requests_remaining=_parse_int(h, "anthropic-ratelimit-requests-remaining"),
            tokens_reset_at=_parse_reset_epoch(h, "anthropic-ratelimit-tokens-reset"),
            requests_reset_at=_parse_reset_epoch(h, "anthropic-ratelimit-requests-reset"),
            raw_headers={k: v for k, v in h.items() if k.startswith("anthropic-ratelimit-")},
        )
