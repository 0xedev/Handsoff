"""handoff CLI entry point.

Talks to the daemon over local HTTP (127.0.0.1:7879) using the JSON-RPC
endpoint defined in handoff.daemon.server. The proxy is a separate process
launched via mitmdump with our addon.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

import httpx
import typer
from rich.console import Console
from rich.table import Table

from handoff.adapters import ALL_ADAPTERS, get_adapter
from handoff.context.engine import ContextEngine, init_project
from handoff.paths import (
    ca_cert_path,
    daemon_pidfile,
    home_dir,
    proxy_pidfile,
)

app = typer.Typer(help="handoff: multi-agent orchestration CLI", no_args_is_help=True)
daemon_app = typer.Typer(help="Manage the handoff daemon")
proxy_app = typer.Typer(help="Manage the local mitmproxy")
brain_app = typer.Typer(help="Edit / view the project brain")
critic_app = typer.Typer(help="Run the cheap-worker / expensive-critic loop")
app.add_typer(daemon_app, name="daemon")
app.add_typer(proxy_app, name="proxy")
app.add_typer(brain_app, name="brain")
app.add_typer(critic_app, name="critic")

console = Console()

DAEMON_HOST = os.environ.get("HANDOFF_DAEMON_HOST", "127.0.0.1")
DAEMON_PORT = int(os.environ.get("HANDOFF_DAEMON_PORT", "7879"))
PROXY_HOST = os.environ.get("HANDOFF_PROXY_HOST", "127.0.0.1")
PROXY_PORT = int(os.environ.get("HANDOFF_PROXY_PORT", "8080"))


# --- helpers --------------------------------------------------------------


def _rpc(method: str, **params) -> dict:
    url = f"http://{DAEMON_HOST}:{DAEMON_PORT}/rpc"
    try:
        r = httpx.post(url, json={"method": method, "params": params}, timeout=5.0)
    except httpx.ConnectError:
        console.print(
            f"[red]Cannot reach daemon at {url}. Run `handoff daemon start` first.[/red]"
        )
        raise typer.Exit(2)
    body = r.json()
    if not body.get("ok"):
        console.print(f"[red]RPC error: {body.get('error')}[/red]")
        raise typer.Exit(1)
    return body["result"]


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


# --- top-level commands ---------------------------------------------------


@app.command()
def init(path: Path = typer.Argument(Path.cwd(), help="Project root")) -> None:
    """Scaffold .handoff/ in the given project."""
    root = path.resolve()
    h = init_project(root)
    try:
        result = _rpc("register_project", root=str(root))
        project_id = result["project_id"]
    except typer.Exit:
        project_id = None
    console.print(f"[green]initialized[/green] {h}")
    if project_id is not None:
        console.print(f"  project_id={project_id}")
    console.print(
        "Next: edit [cyan].handoff/brain.md[/cyan] then run [cyan]handoff sync[/cyan]."
    )


@app.command()
def sync(path: Path = typer.Argument(Path.cwd(), help="Project root")) -> None:
    """Render brain.md into per-agent context files."""
    engine = ContextEngine(path.resolve())
    written = engine.sync()
    for p in written:
        console.print(f"[green]wrote[/green] {p}")


@app.command()
def agents(project_id: Optional[int] = None) -> None:
    """List running agents with their latest rate-limit state."""
    result = _rpc("list_agents", project_id=project_id)
    rows = result["agents"]
    if not rows:
        console.print("[dim]no agents tracked yet[/dim]")
        return
    table = Table(title="agents")
    table.add_column("id", justify="right")
    table.add_column("kind")
    table.add_column("pid", justify="right")
    table.add_column("status")
    table.add_column("tokens_remaining", justify="right")
    table.add_column("requests_remaining", justify="right")
    table.add_column("last_sample")
    now = int(time.time())
    for r in rows:
        last = r["last_sample_ts"]
        last_str = f"{now - last}s ago" if last else "-"
        table.add_row(
            str(r["id"]),
            r["kind"],
            str(r["pid"] or "-"),
            r["status"],
            str(r["tokens_remaining"] if r["tokens_remaining"] is not None else "-"),
            str(r["requests_remaining"] if r["requests_remaining"] is not None else "-"),
            last_str,
        )
    console.print(table)


@app.command()
def spawn(
    kind: str = typer.Argument(..., help="claude | codex | copilot | cursor"),
    args: list[str] = typer.Argument(None, help="Extra args passed to the agent"),
    project: Path = typer.Option(Path.cwd(), help="Project root"),
    no_proxy: bool = typer.Option(False, help="Skip wiring HTTPS_PROXY"),
) -> None:
    """Spawn an agent with proxy env wired and register it with the daemon."""
    adapter = get_adapter(kind)
    project_root = project.resolve()
    env: dict[str, str] = {}
    if not no_proxy:
        proxy_url = f"http://{PROXY_HOST}:{PROXY_PORT}"
        env["HTTP_PROXY"] = proxy_url
        env["HTTPS_PROXY"] = proxy_url
        env["http_proxy"] = proxy_url
        env["https_proxy"] = proxy_url
        ca = ca_cert_path()
        if ca.exists():
            env["SSL_CERT_FILE"] = str(ca)
            env["REQUESTS_CA_BUNDLE"] = str(ca)
            env["NODE_EXTRA_CA_CERTS"] = str(ca)
        else:
            console.print(
                f"[yellow]warning:[/yellow] mitmproxy CA not found at {ca}. "
                "Run `handoff proxy start` first or use --no-proxy."
            )

    proc = adapter.spawn(project_root, env=env, extra_args=list(args or []))
    res = _rpc(
        "register_agent",
        kind=adapter.kind,
        pid=proc.pid,
        spawned_by="handoff",
    )
    console.print(
        f"[green]spawned[/green] {kind} pid={proc.pid} agent_id={res['agent_id']}"
    )
    try:
        proc.wait()
    except KeyboardInterrupt:
        proc.terminate()
    finally:
        _rpc("stop_agent", agent_id=res["agent_id"])


# --- daemon ---------------------------------------------------------------


@daemon_app.command("start")
def daemon_start() -> None:
    """Start the handoff daemon as a background process."""
    pidfile = daemon_pidfile()
    if pidfile.exists():
        try:
            pid = int(pidfile.read_text().strip())
            if _pid_alive(pid):
                console.print(f"[yellow]daemon already running[/yellow] (pid={pid})")
                return
        except ValueError:
            pass

    log_path = home_dir() / "daemon.log"
    cmd = [
        sys.executable,
        "-m",
        "uvicorn",
        "handoff.daemon.server:build_app",
        "--factory",
        "--host",
        DAEMON_HOST,
        "--port",
        str(DAEMON_PORT),
        "--log-level",
        "info",
    ]
    log_fh = open(log_path, "ab")
    proc = subprocess.Popen(
        cmd,
        stdout=log_fh,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    pidfile.write_text(str(proc.pid))
    # wait briefly for /health
    for _ in range(20):
        time.sleep(0.1)
        try:
            r = httpx.get(f"http://{DAEMON_HOST}:{DAEMON_PORT}/health", timeout=0.5)
            if r.status_code == 200:
                console.print(f"[green]daemon up[/green] pid={proc.pid} log={log_path}")
                return
        except httpx.HTTPError:
            continue
    console.print(f"[red]daemon failed to come up[/red]; check {log_path}")


@daemon_app.command("stop")
def daemon_stop() -> None:
    pidfile = daemon_pidfile()
    if not pidfile.exists():
        console.print("[dim]no daemon pidfile[/dim]")
        return
    try:
        pid = int(pidfile.read_text().strip())
    except ValueError:
        pidfile.unlink(missing_ok=True)
        return
    try:
        os.kill(pid, signal.SIGTERM)
        console.print(f"[green]sent SIGTERM[/green] to {pid}")
    except OSError as e:
        console.print(f"[yellow]{e}[/yellow]")
    pidfile.unlink(missing_ok=True)


@daemon_app.command("status")
def daemon_status() -> None:
    try:
        r = httpx.get(f"http://{DAEMON_HOST}:{DAEMON_PORT}/health", timeout=1.0)
        console.print(f"[green]up[/green] {r.json()}")
    except httpx.HTTPError:
        console.print("[red]down[/red]")


# --- proxy ----------------------------------------------------------------


@proxy_app.command("start")
def proxy_start() -> None:
    """Start mitmdump with the handoff addon loaded."""
    pidfile = proxy_pidfile()
    if pidfile.exists():
        try:
            pid = int(pidfile.read_text().strip())
            if _pid_alive(pid):
                console.print(f"[yellow]proxy already running[/yellow] (pid={pid})")
                return
        except ValueError:
            pass

    from handoff.proxy import addon as addon_mod

    addon_path = os.path.abspath(addon_mod.__file__)
    log_path = home_dir() / "proxy.log"
    cmd = [
        "mitmdump",
        "-s",
        addon_path,
        "--listen-host",
        PROXY_HOST,
        "--listen-port",
        str(PROXY_PORT),
        "--set",
        "console_eventlog_verbosity=warn",
    ]
    try:
        log_fh = open(log_path, "ab")
        proc = subprocess.Popen(
            cmd,
            stdout=log_fh,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    except FileNotFoundError:
        console.print("[red]mitmdump not on PATH[/red]; pip install mitmproxy")
        raise typer.Exit(2)
    pidfile.write_text(str(proc.pid))
    console.print(f"[green]proxy up[/green] pid={proc.pid} {PROXY_HOST}:{PROXY_PORT}")
    ca = ca_cert_path()
    if not ca.exists():
        console.print(
            "[yellow]first run:[/yellow] mitmproxy will generate its CA on first request. "
            "After that, install it system-wide so spawned agents trust HTTPS:\n"
            "  macOS:  sudo security add-trusted-cert -d -r trustRoot "
            "-k /Library/Keychains/System.keychain ~/.mitmproxy/mitmproxy-ca-cert.pem\n"
            "  Linux:  sudo cp ~/.mitmproxy/mitmproxy-ca-cert.pem "
            "/usr/local/share/ca-certificates/mitmproxy.crt && sudo update-ca-certificates"
        )


@proxy_app.command("stop")
def proxy_stop() -> None:
    pidfile = proxy_pidfile()
    if not pidfile.exists():
        console.print("[dim]no proxy pidfile[/dim]")
        return
    try:
        pid = int(pidfile.read_text().strip())
    except ValueError:
        pidfile.unlink(missing_ok=True)
        return
    try:
        os.kill(pid, signal.SIGTERM)
        console.print(f"[green]sent SIGTERM[/green] to {pid}")
    except OSError as e:
        console.print(f"[yellow]{e}[/yellow]")
    pidfile.unlink(missing_ok=True)


@proxy_app.command("status")
def proxy_status() -> None:
    pidfile = proxy_pidfile()
    if not pidfile.exists():
        console.print("[red]down[/red]")
        return
    try:
        pid = int(pidfile.read_text().strip())
    except ValueError:
        console.print("[red]down (bad pidfile)[/red]")
        return
    if _pid_alive(pid):
        console.print(f"[green]up[/green] pid={pid} {PROXY_HOST}:{PROXY_PORT}")
    else:
        console.print(f"[red]down[/red] (stale pidfile pid={pid})")


# --- brain ----------------------------------------------------------------


@brain_app.command("cat")
def brain_cat(project: Path = typer.Argument(Path.cwd())) -> None:
    p = project.resolve() / ".handoff" / "brain.md"
    if not p.exists():
        console.print(f"[red]no brain at {p}; run `handoff init`[/red]")
        raise typer.Exit(2)
    console.print(p.read_text())


@brain_app.command("edit")
def brain_edit(project: Path = typer.Argument(Path.cwd())) -> None:
    p = project.resolve() / ".handoff" / "brain.md"
    if not p.exists():
        console.print(f"[red]no brain at {p}; run `handoff init`[/red]")
        raise typer.Exit(2)
    editor = os.environ.get("EDITOR", "vi")
    subprocess.call([editor, str(p)])


@brain_app.command("append")
def brain_append(
    text: str = typer.Argument(..., help="Text to append"),
    project: Path = typer.Option(Path.cwd()),
) -> None:
    p = project.resolve() / ".handoff" / "brain.md"
    if not p.exists():
        console.print(f"[red]no brain at {p}; run `handoff init`[/red]")
        raise typer.Exit(2)
    with p.open("a") as f:
        f.write("\n" + text.rstrip() + "\n")
    console.print(f"[green]appended[/green] to {p}")


# --- discover (one-shot ps scan) -----------------------------------------


@app.command()
def attach(
    pid: int = typer.Argument(..., help="PID of the agent process to register"),
    kind: str = typer.Option(..., help="claude | codex | copilot | cursor"),
    project: Path = typer.Option(Path.cwd(), help="Project root"),
) -> None:
    """Register an already-running agent so the daemon tracks it."""
    _rpc("register_project", root=str(project.resolve()))
    res = _rpc("attach_agent", kind=kind, pid=pid)
    console.print(
        f"[green]attached[/green] {kind} pid={pid} agent_id={res['agent_id']}"
    )


@app.command(name="handoff")
def handoff_now(
    to_kind: str = typer.Argument(..., help="Agent kind to hand off to"),
    from_agent: Optional[int] = typer.Option(
        None, "--from", help="agent_id of the failing agent (optional)"
    ),
    reason: str = typer.Option("manual", help="Reason logged for the handoff"),
    no_spawn: bool = typer.Option(False, help="Snapshot only; don't spawn the new agent"),
    project: Path = typer.Option(Path.cwd(), help="Project root"),
) -> None:
    """Snapshot context and (optionally) spawn the next agent."""
    _rpc("register_project", root=str(project.resolve()))
    res = _rpc(
        "handoff",
        from_agent_id=from_agent,
        to_kind=to_kind,
        reason=reason,
        auto_spawn=not no_spawn,
    )
    console.print(f"[green]handoff[/green] -> {to_kind}")
    console.print(f"  snapshot: {res.get('snapshot_path')}")
    if res.get("to_agent_id"):
        console.print(
            f"  spawned agent_id={res['to_agent_id']} pid={res.get('to_pid')}"
        )
    else:
        console.print("  [dim]no agent spawned[/dim]")


@critic_app.command("watch")
def critic_watch(
    task: str = typer.Argument(..., help="Default task description for re-runs"),
    project: Path = typer.Option(Path.cwd(), help="Project root"),
    worker_model: str = typer.Option("claude-haiku-4-5-20251001"),
    critic_model: str = typer.Option("claude-opus-4-7"),
    interval: float = typer.Option(2.0, help="Poll interval (seconds)"),
    debounce: float = typer.Option(3.0, help="Debounce window (seconds)"),
    no_proxy: bool = typer.Option(False),
) -> None:
    """Re-run the critic loop whenever tracked files change."""
    from handoff.critic.runner import CriticRunner
    from handoff.critic.watch import WatchLoop

    proxy = None if no_proxy else f"http://{PROXY_HOST}:{PROXY_PORT}"
    root = project.resolve()
    runner = CriticRunner(
        root,
        worker_model=worker_model,
        critic_model=critic_model,
        proxy_url=proxy,
    )

    def fire(changed: set[Path]) -> None:
        names = sorted({p.name for p in changed})[:5]
        console.print(f"[cyan]changed:[/cyan] {', '.join(names)}")
        try:
            result = runner.run(task)
        except Exception as e:  # noqa: BLE001
            console.print(f"[red]critic run failed:[/red] {e}")
            return
        color = "green" if result.verdict == "approve" else "yellow"
        console.print(
            f"  [{color}]{result.verdict}[/{color}] "
            f"worker={result.worker_tokens} critic={result.critic_tokens}"
        )
        try:
            _rpc(
                "record_critic_run",
                worker_model=worker_model,
                critic_model=critic_model,
                worker_tokens=result.worker_tokens,
                critic_tokens=result.critic_tokens,
                verdict=result.verdict,
                notes=result.notes,
            )
        except typer.Exit:
            pass

    loop = WatchLoop(root, fire, interval=interval, debounce=debounce)
    console.print(f"[green]watching[/green] {root} (Ctrl-C to stop)")
    loop.run_forever()


@critic_app.command("run")
def critic_run(
    task: str = typer.Argument(..., help="Task description for the worker model"),
    project: Path = typer.Option(Path.cwd(), help="Project root"),
    worker_model: str = typer.Option("claude-haiku-4-5-20251001"),
    critic_model: str = typer.Option("claude-opus-4-7"),
    no_proxy: bool = typer.Option(
        False, help="Don't route through the local proxy (skips usage tracking)"
    ),
) -> None:
    """One-shot critic loop: worker drafts a diff, critic reviews."""
    from handoff.critic.runner import CriticRunner

    proxy = None if no_proxy else f"http://{PROXY_HOST}:{PROXY_PORT}"
    runner = CriticRunner(
        project.resolve(),
        worker_model=worker_model,
        critic_model=critic_model,
        proxy_url=proxy,
    )
    result = runner.run(task)

    color = "green" if result.verdict == "approve" else "yellow"
    console.print(f"[{color}]verdict:[/{color}] {result.verdict}")
    console.print(f"notes: {result.notes}")
    console.print(
        f"tokens: worker={result.worker_tokens} critic={result.critic_tokens}"
    )
    for p in result.artifacts:
        console.print(f"  wrote {p}")

    try:
        _rpc(
            "record_critic_run",
            worker_model=worker_model,
            critic_model=critic_model,
            worker_tokens=result.worker_tokens,
            critic_tokens=result.critic_tokens,
            verdict=result.verdict,
            notes=result.notes,
        )
    except typer.Exit:
        # Daemon down is not fatal for a one-shot run.
        pass


@app.command()
def discover() -> None:
    """Scan running processes for known agents (does not register them)."""
    import psutil

    procs: list[dict] = []
    for p in psutil.process_iter(["pid", "name", "cmdline"]):
        try:
            procs.append(p.info)
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue

    table = Table(title="discovered agents")
    table.add_column("kind")
    table.add_column("pid", justify="right")
    table.add_column("name")
    table.add_column("cmd")
    found = 0
    for adapter in ALL_ADAPTERS:
        for m in adapter.detect(procs):
            cmd = " ".join(m.cmdline)[:80]
            table.add_row(adapter.kind, str(m.pid), m.name, cmd)
            found += 1
    if found:
        console.print(table)
    else:
        console.print("[dim]no known agents running[/dim]")


if __name__ == "__main__":  # pragma: no cover
    app()
