"""Headless agent spawn for failover.

When the failover policy fires, the daemon needs to launch a fresh agent in
non-interactive mode and feed it a snapshot prompt. Each agent has a
different one-shot CLI form:
    claude:  claude -p "<prompt>"
    codex:   codex exec "<prompt>"
    copilot: gh copilot suggest "<prompt>"
    cursor:  not supported (IDE-only)
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path
from typing import Optional

from handoff.adapters import get_adapter
from handoff.paths import ca_cert_path, home_dir

NON_INTERACTIVE_ARGV: dict[str, list[str]] = {
    "claude": ["-p"],
    "codex": ["exec"],
    "copilot": ["copilot", "suggest"],  # `gh copilot suggest <prompt>`
}


def proxy_env(proxy_url: str) -> dict[str, str]:
    env = {
        "HTTP_PROXY": proxy_url,
        "HTTPS_PROXY": proxy_url,
        "http_proxy": proxy_url,
        "https_proxy": proxy_url,
    }
    ca = ca_cert_path()
    if ca.exists():
        env["SSL_CERT_FILE"] = str(ca)
        env["REQUESTS_CA_BUNDLE"] = str(ca)
        env["NODE_EXTRA_CA_CERTS"] = str(ca)
    return env


def _resolve_binary(adapter) -> Optional[str]:
    for b in adapter.binaries:
        path = shutil.which(b)
        if path:
            return path
    return None


def headless_spawn(
    kind: str,
    project_root: Path,
    prompt: str,
    proxy_url: str = "http://127.0.0.1:8080",
) -> Optional[subprocess.Popen]:
    """Spawn an agent in one-shot mode with `prompt` as input. Returns None if
    the agent has no headless mode (e.g. cursor)."""
    adapter = get_adapter(kind)
    if kind not in NON_INTERACTIVE_ARGV:
        return None
    binary = _resolve_binary(adapter)
    if binary is None:
        return None

    env = os.environ.copy()
    env.update(proxy_env(proxy_url))

    # gh copilot suggest is `gh copilot suggest <prompt>`; binary already 'gh'
    extra = NON_INTERACTIVE_ARGV[kind]
    if kind == "copilot" and Path(binary).name == "gh":
        argv = [binary, *extra, prompt]
    elif kind == "copilot":
        argv = [binary, "suggest", prompt]
    else:
        argv = [binary, *extra, prompt]

    log_path = home_dir() / f"agent-{kind}-{int(__import__('time').time())}.log"
    log_fh = open(log_path, "ab")
    return subprocess.Popen(
        argv,
        cwd=str(project_root),
        env=env,
        stdout=log_fh,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
