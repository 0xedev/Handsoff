# handoff

Multi-agent orchestration CLI for AI coding agents (Claude Code, Codex CLI, Copilot CLI, Cursor/Antigravity).

`handoff` runs a local daemon and HTTP proxy that:

- detects which AI coding agents are running on your machine
- meters each one's API usage in real time by reading provider rate-limit headers
- shares a single markdown "brain" across all of them
- hands off to a fresh agent when the active one approaches its limit (v0.2)
- runs an internal cheap-worker / expensive-critic loop (v0.2)

## Status

v0.1 scaffold. See `/root/.claude/plans/i-m-thinking-of-building-eventual-dijkstra.md` for the full plan.

## Quick start

```bash
pipx install handoff
handoff init
handoff daemon start
handoff proxy start          # follow CA-install prompt
handoff spawn claude -- "summarize this repo"
handoff agents               # live table of agents + remaining tokens
```

## Architecture

```
   shell  ──▶  handoff (CLI)  ──unix sock──▶  handoffd
                                                │
        AgentRegistry ── ContextEngine ── CriticRunner
                          │
                    SQLite ~/.handoff/state.db
                          ▲
                          │ POST /ingest
                          │
                  handoff-proxy (mitmdump addon)
                          ▲
                          │ HTTPS_PROXY=127.0.0.1:8080
                  claude / codex / copilot / cursor
```

Linux + macOS only in v0.1. Windows via WSL.
