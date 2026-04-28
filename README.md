# handoff

Multi-agent orchestration CLI for AI coding agents (Claude Code, Codex CLI,
Copilot CLI, Cursor / Antigravity). Single-binary Rust.

`handoff` runs a local daemon and an HTTPS-MITM proxy that:

- **observes** which AI coding agents are running on your machine
- **meters** each one's API usage in real time by reading provider
  rate-limit headers (`anthropic-ratelimit-*`, `x-ratelimit-*`)
- **shares** a single project brain across all of them
- **fails over** to a fresh agent when the active one approaches its limit,
  with a critic-summarised handoff brief
- **runs** an internal cheap-worker / expensive-critic loop when you want supervised autonomous edits

**handoff proxy + failover** require no API key — they observe rate-limit headers and redirect your agent processes.

**handoff critic** uses the Anthropic API (set `ANTHROPIC_API_KEY`). Worker model: `claude-haiku-4-5`, Critic: `claude-opus-4`.

## Status

v0.4.1-alpha — alpha-bugfix release.
47 tests, all passing.

What's new since v0.4.0-alpha:
- **macOS PID attribution fix.** `lsof -i:<port>` returns both ends of
  a localhost MITM connection; we now match by **local** endpoint so
  the agent's PID gets recorded, not the proxy's own.
- **`handoff spawn` headless behaviour.** Positional args after `--`
  now invoke headless mode (`claude -p "summarize"`); no args = TUI.
- **`handoff doctor`.** Preflight check for daemon/proxy/CA/agents.

## Quick start

```bash
cargo install --path rust/crates/cli           # builds the `handoff` binary

handoff init                                    # scaffold .handoff/
handoff daemon start                            # background daemon
handoff proxy start                             # local MITM proxy
                                                # (CA install prompt on first run)
handoff spawn claude -- "summarize this repo"   # headless: claude -p "..."
handoff spawn claude                            # interactive TUI (no args)
handoff agents                                  # live table of agents + tokens

handoff critic run "add a /version endpoint"    # local-CLI worker + critic
handoff critic run "X" --worker codex --critic claude
handoff critic watch "polish the docs"          # re-runs on file changes

handoff doctor                                  # preflight check
```

## Architecture

```
   shell  ──▶  handoff (CLI)  ──HTTP──▶  handoffd (axum)
                                            │
        ┌───────────────────────────────────┼───────────────────────┐
        │                                   │                       │
   AgentRegistry                       ContextEngine            FailoverEngine
   (sysinfo + proxy-classified)        (.handoff/ brain)         (mpsc channel)
        │                                   │                       │
        └────────────── SQLite (~/.handoff/state.db) ────────────────┘
                                ▲
                                │ POST /ingest (headers + PID + sample)
                                │
                       handoff-proxy (hudsucker MITM)
                                ▲
                                │ HTTPS_PROXY=127.0.0.1:8080
        ┌───────────────────────┼─────────────────────────┐
     claude                  codex                  copilot         cursor/antigravity
                                                                    (companion extension)
```

## Privacy

- The MITM proxy logs response **headers only** — never request or response bodies.
- Rate-limit headers (`anthropic-ratelimit-*`, `x-ratelimit-*`) are stored in `~/.handoff/state.db`.
- Snapshots written to `<project>/.handoff/scratch/` are local-only; nothing is sent to any server.
- To delete all state: `rm -rf ~/.handoff && rm -rf <project>/.handoff`

## Workspace layout

```
rust/
  Cargo.toml                  workspace, edition 2021, rustc 1.80+
  crates/
    cli/        clap front-end
    daemon/     axum HTTP + ingest + RPC + failover engine
    proxy/      hudsucker MITM + on-disk CA + PID-from-socket lookup
    context/    brain.md + intelligent Snapshot from git/shell/tests
    critic/     reqwest → Anthropic worker/critic/summariser + watch loop
    policy/     declarative thresholds + chain
    adapters/   per-agent detection + rate-limit header parsing
    storage/    rusqlite schema + queries
    common/     shared types + paths
extension/      VSCode/Cursor companion (TypeScript) — TLS-pinning fallback
```

## Per-agent support

| Agent | Detect | Context inject | Usage read | Headless spawn |
|---|---|---|---|---|
| **Claude Code** | `claude` binary; proxy: `api.anthropic.com` | `CLAUDE.md` | `anthropic-ratelimit-*` | `claude -p "<prompt>"` |
| **Codex CLI** | `codex` binary; proxy: `api.openai.com` | `AGENTS.md` | `x-ratelimit-*` | `codex exec "<prompt>"` |
| **Copilot CLI** | `gh copilot`; proxy: `api.githubcopilot.com` | `.github/copilot-instructions.md` | request counts only | `gh copilot suggest "<prompt>"` |
| **Cursor / Antigravity** | Electron binary | `.cursorrules` | best-effort (TLS-pinned; companion extension) | not supported |

## CLI surface

```
handoff init [path]                         scaffold .handoff/
handoff sync [path]                         brain.md → derived/*
handoff agents                              live agent table
handoff discover                            scan running processes
handoff snapshot [--reason] [--json]        generate intelligent Snapshot
handoff spawn <kind> [--no-proxy] -- ...    spawn agent w/ proxy env
handoff attach <pid> --kind=<kind>          register existing process
handoff handoff <to-kind> [--from N]        manual failover
handoff brain {cat|edit|append}             brain.md helpers
handoff critic run "<task>"                 one-shot worker+critic
handoff critic watch "<task>"               re-run on file changes
handoff daemon {run|start|stop|status}      daemon lifecycle
handoff proxy {start|stop|status}           proxy lifecycle
```

## Per-project config (`.handoff/config.toml`)

```toml
[failover]
tokens_remaining_pct = 10.0       # trigger when % drops below
tokens_remaining_abs = 1000       # OR absolute tokens
requests_remaining = 5            # OR remaining requests
chain = ["claude", "codex", "copilot"]
auto_spawn = true
summarize = true                  # use critic model for handoff brief

[critic]
worker_agent     = "claude"        # local CLI: claude | codex | copilot
critic_agent     = "claude"
summarizer_agent = "claude"
# v0.4.0-alpha names (worker_model / critic_model / summarizer_model)
# are still accepted as aliases.
```

## Risks / open issues

1. **CA install** is a real onboarding wart. `handoff proxy start` prints
   the per-OS install command on first run.
2. **Cursor / Antigravity** pin TLS; the companion extension under
   `extension/` is the activity-signal fallback (no token budget metering).
3. **Copilot** is opaque (no per-request budget header); we count requests
   and 429s only.

## Build / test

```bash
cd rust
cargo build --workspace --release
cargo test --workspace
```

Linux + macOS only. Windows via WSL.
