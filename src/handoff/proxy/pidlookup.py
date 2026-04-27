"""Map a TCP socket (local addr, port) -> owning PID.

Linux:   parse /proc/net/tcp + scan /proc/<pid>/fd/* for matching socket inode.
macOS:   shell out to `lsof -i -n -P -F pn`.
Other:   not supported in v0.1.
"""

from __future__ import annotations

import os
import platform
import re
import subprocess
from functools import lru_cache
from typing import Optional


def lookup_pid(local_ip: str, local_port: int) -> Optional[int]:
    sysname = platform.system()
    if sysname == "Linux":
        return _lookup_linux(local_ip, local_port)
    if sysname == "Darwin":
        return _lookup_macos(local_ip, local_port)
    return None


# --- Linux -----------------------------------------------------------------


def _hex_addr(ip: str, port: int) -> str:
    parts = ip.split(".")
    if len(parts) != 4:
        return ""
    hex_ip = "".join(f"{int(p):02X}" for p in reversed(parts))
    return f"{hex_ip}:{port:04X}"


def _read_proc_net_tcp() -> list[tuple[str, str, str]]:
    """Return list of (local_addr_hex, remote_addr_hex, inode)."""
    out: list[tuple[str, str, str]] = []
    for path in ("/proc/net/tcp", "/proc/net/tcp6"):
        try:
            with open(path) as f:
                next(f, None)  # header
                for line in f:
                    parts = line.split()
                    if len(parts) < 10:
                        continue
                    out.append((parts[1], parts[2], parts[9]))
        except FileNotFoundError:
            continue
    return out


def _lookup_linux(local_ip: str, local_port: int) -> Optional[int]:
    target = _hex_addr(local_ip, local_port)
    if not target:
        return None
    inode: Optional[str] = None
    for laddr, _raddr, ino in _read_proc_net_tcp():
        if laddr.upper() == target.upper():
            inode = ino
            break
    if not inode or inode == "0":
        return None
    needle = f"socket:[{inode}]"
    for pid_str in os.listdir("/proc"):
        if not pid_str.isdigit():
            continue
        fd_dir = f"/proc/{pid_str}/fd"
        try:
            for fd in os.listdir(fd_dir):
                try:
                    if os.readlink(f"{fd_dir}/{fd}") == needle:
                        return int(pid_str)
                except OSError:
                    continue
        except (PermissionError, FileNotFoundError):
            continue
    return None


# --- macOS -----------------------------------------------------------------

_LSOF_LINE = re.compile(r"^p(\d+)$")


@lru_cache(maxsize=1)
def _have_lsof() -> bool:
    try:
        subprocess.run(["lsof", "-v"], capture_output=True, timeout=2)
        return True
    except (FileNotFoundError, subprocess.SubprocessError):
        return False


def _lookup_macos(local_ip: str, local_port: int) -> Optional[int]:
    if not _have_lsof():
        return None
    try:
        res = subprocess.run(
            ["lsof", "-iTCP", f"-i:{local_port}", "-n", "-P", "-F", "pn"],
            capture_output=True,
            text=True,
            timeout=2,
        )
    except subprocess.SubprocessError:
        return None
    pid: Optional[int] = None
    for line in res.stdout.splitlines():
        m = _LSOF_LINE.match(line)
        if m:
            pid = int(m.group(1))
        elif line.startswith("n") and pid is not None:
            # Match local endpoint
            if f"{local_ip}:{local_port}" in line or f"*:{local_port}" in line:
                return pid
    return pid
