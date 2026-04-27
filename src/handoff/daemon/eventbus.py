from __future__ import annotations

from collections import defaultdict
from typing import Any, Callable

Listener = Callable[[dict[str, Any]], None]


class EventBus:
    """Tiny synchronous in-process pub/sub. Sufficient for v0.1."""

    def __init__(self) -> None:
        self._listeners: dict[str, list[Listener]] = defaultdict(list)

    def subscribe(self, topic: str, fn: Listener) -> None:
        self._listeners[topic].append(fn)

    def publish(self, topic: str, payload: dict[str, Any]) -> None:
        for fn in list(self._listeners.get(topic, ())):
            try:
                fn(payload)
            except Exception:  # noqa: BLE001
                # Don't let one buggy listener break the bus.
                continue
