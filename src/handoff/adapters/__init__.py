from handoff.adapters.base import Adapter, ProcessMatch
from handoff.adapters.claude import ClaudeAdapter
from handoff.adapters.codex import CodexAdapter
from handoff.adapters.copilot import CopilotAdapter
from handoff.adapters.cursor import CursorAdapter

ALL_ADAPTERS: list[Adapter] = [
    ClaudeAdapter(),
    CodexAdapter(),
    CopilotAdapter(),
    CursorAdapter(),
]


def get_adapter(kind: str) -> Adapter:
    for a in ALL_ADAPTERS:
        if a.kind == kind:
            return a
    raise KeyError(f"unknown agent kind: {kind}")


__all__ = [
    "Adapter",
    "ProcessMatch",
    "ALL_ADAPTERS",
    "get_adapter",
    "ClaudeAdapter",
    "CodexAdapter",
    "CopilotAdapter",
    "CursorAdapter",
]
