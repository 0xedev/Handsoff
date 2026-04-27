"""mitmproxy addon: classify requests by agent, parse rate-limit headers, POST to daemon.

Run via:
    mitmdump -s $(python -c "import handoff.proxy.addon, os; \
                              print(os.path.abspath(handoff.proxy.addon.__file__))") \
             --listen-host 127.0.0.1 --listen-port 8080
"""

from __future__ import annotations

import json
import logging
import os
from dataclasses import asdict
from typing import Optional

import httpx

from handoff.adapters import ALL_ADAPTERS
from handoff.proxy.pidlookup import lookup_pid

log = logging.getLogger("handoff.proxy")

DAEMON_INGEST_URL = os.environ.get(
    "HANDOFF_INGEST_URL", "http://127.0.0.1:7879/ingest"
)


def _classify_kind(host: str) -> Optional[str]:
    for adapter in ALL_ADAPTERS:
        if adapter.api_hosts and adapter.classify_host(host):
            return adapter.kind
    return None


def _parse_sample(kind: str, headers: dict[str, str]) -> Optional[dict]:
    for adapter in ALL_ADAPTERS:
        if adapter.kind != kind:
            continue
        sample = adapter.parse_headers(headers)
        if sample is None:
            return None
        return asdict(sample)
    return None


class HandoffAddon:
    """One-method addon: on every response, post sample to daemon."""

    def __init__(self) -> None:
        self._client = httpx.Client(timeout=2.0)

    def response(self, flow) -> None:  # pragma: no cover - exercised via mitmdump
        try:
            req = flow.request
            resp = flow.response
            host = req.pretty_host
            kind = _classify_kind(host)
            if kind is None:
                return

            headers = {k.lower(): v for k, v in resp.headers.items()}
            sample = _parse_sample(kind, headers)

            pid: Optional[int] = None
            try:
                client_addr = flow.client_conn.peername
                if client_addr:
                    pid = lookup_pid(client_addr[0], int(client_addr[1]))
            except Exception:  # noqa: BLE001
                pid = None

            payload = {
                "kind": kind,
                "host": host,
                "status_code": resp.status_code,
                "pid": pid,
                "sample": sample,
            }
            try:
                self._client.post(DAEMON_INGEST_URL, json=payload)
            except httpx.HTTPError as e:
                log.debug("ingest post failed: %s", e)
        except Exception as e:  # noqa: BLE001
            log.warning("addon error: %s", e)


addons = [HandoffAddon()]


def run() -> None:
    """Entry point for `handoff-proxy-addon` script (prints addon path)."""
    print(json.dumps({"addon_path": os.path.abspath(__file__)}))
