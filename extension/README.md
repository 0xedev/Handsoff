# handoff companion

VSCode / Cursor / Antigravity extension that posts editor activity to the
local `handoff` daemon. This is the TLS-pinning fallback for the
multi-agent orchestrator — Electron-based IDEs pin their TLS roots so the
local mitmproxy can't read their model traffic. The companion reports what
it _can_ see (heartbeats + edit pulses) so those agents at least show up
in `handoff agents`.

## What it reports

- **Heartbeat** every `handoff.heartbeatSeconds` (default 30s):
  `POST /ingest {kind, status_code: 200, pid: <process.pid>, sample: null}`.
  Marks the editor as "active" so it appears in `handoff agents`.
- **Edit pulse** whenever a non-user text-document change lands quickly
  enough to look like an AI suggestion acceptance (heuristic, not exact).
  Logged the same way; the daemon counts these in `total_requests`.
- **Manual usage report** via the `handoff: report usage` command — opens
  a quick-pick to paste counts from the IDE's account page when you have
  them.

## Build + install

```bash
cd extension
npm install
npm run build
npm run package          # produces handoff-companion.vsix
code --install-extension handoff-companion.vsix
```

(Replace `code` with `cursor` or `antigravity` as appropriate.)

## Configuration

```jsonc
{
  "handoff.daemonUrl": "http://127.0.0.1:7879",
  "handoff.kind": "cursor",
  "handoff.heartbeatSeconds": 30
}
```

## Limitations

- **No real usage metering.** Cursor and Antigravity don't expose token
  counts to extensions. You get _activity signal_, not _budget signal_.
  Failover from these agents will only fire on a manual report or 429s
  surfaced via `handoff.reportUsage`.
- VSCode + Copilot *does* expose `vscode.lm` token counts; a future
  version of this extension should wire those through.
