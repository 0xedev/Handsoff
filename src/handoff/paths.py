from __future__ import annotations

import os
from pathlib import Path


def home_dir() -> Path:
    p = Path(os.environ.get("HANDOFF_HOME", Path.home() / ".handoff"))
    p.mkdir(parents=True, exist_ok=True)
    return p


def db_path() -> Path:
    return home_dir() / "state.db"


def daemon_socket() -> Path:
    return home_dir() / "daemon.sock"


def daemon_pidfile() -> Path:
    return home_dir() / "daemon.pid"


def proxy_pidfile() -> Path:
    return home_dir() / "proxy.pid"


def ca_cert_path() -> Path:
    return Path.home() / ".mitmproxy" / "mitmproxy-ca-cert.pem"


def project_dir(root: Path) -> Path:
    return root / ".handoff"
