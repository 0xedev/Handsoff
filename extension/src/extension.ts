import * as http from "http";
import { URL } from "url";
import * as vscode from "vscode";

interface IngestPayload {
  kind: string;
  host: string;
  status_code: number;
  pid: number | null;
  sample: Record<string, unknown> | null;
}

function postIngest(daemonUrl: string, payload: IngestPayload): void {
  let target: URL;
  try {
    target = new URL("/ingest", daemonUrl);
  } catch (e) {
    console.warn("handoff: bad daemonUrl", daemonUrl, e);
    return;
  }
  const body = Buffer.from(JSON.stringify(payload), "utf8");
  const req = http.request(
    {
      hostname: target.hostname,
      port: target.port || 80,
      path: target.pathname,
      method: "POST",
      headers: {
        "content-type": "application/json",
        "content-length": body.length.toString(),
      },
      timeout: 2000,
    },
    (res) => {
      // Drain to free the socket.
      res.resume();
    }
  );
  req.on("error", (e) => {
    // Daemon not running is the common case; debug log only.
    console.debug("handoff ingest error:", e.message);
  });
  req.on("timeout", () => {
    req.destroy();
  });
  req.write(body);
  req.end();
}

function readConfig(): { daemonUrl: string; kind: string; heartbeatSeconds: number } {
  const cfg = vscode.workspace.getConfiguration("handoff");
  return {
    daemonUrl: cfg.get<string>("daemonUrl", "http://127.0.0.1:7879"),
    kind: cfg.get<string>("kind", "cursor"),
    heartbeatSeconds: cfg.get<number>("heartbeatSeconds", 30),
  };
}

export function activate(context: vscode.ExtensionContext): void {
  const out = vscode.window.createOutputChannel("handoff");
  out.appendLine(`handoff companion activated (pid=${process.pid})`);

  const sendHeartbeat = (statusCode: number = 200): void => {
    const { daemonUrl, kind } = readConfig();
    postIngest(daemonUrl, {
      kind,
      host: "vscode-extension",
      status_code: statusCode,
      pid: process.pid,
      sample: null,
    });
  };

  // Initial heartbeat + interval
  sendHeartbeat();
  let interval = setInterval(
    sendHeartbeat,
    Math.max(5, readConfig().heartbeatSeconds) * 1000
  );

  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("handoff.heartbeatSeconds")) {
        clearInterval(interval);
        interval = setInterval(
          sendHeartbeat,
          Math.max(5, readConfig().heartbeatSeconds) * 1000
        );
      }
    })
  );

  // Heuristic edit pulse: large insertions (>= 40 chars) that arrive in a
  // single content change look like AI suggestion acceptances. Not exact,
  // but the only signal we get without a real LM API.
  context.subscriptions.push(
    vscode.workspace.onDidChangeTextDocument((e) => {
      for (const change of e.contentChanges) {
        if (change.text.length >= 40 && !change.text.includes("\b")) {
          sendHeartbeat(200);
          return;
        }
      }
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("handoff.ping", () => {
      sendHeartbeat();
      vscode.window.showInformationMessage("handoff: pinged daemon");
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("handoff.reportUsage", async () => {
      const remaining = await vscode.window.showInputBox({
        prompt: "Tokens remaining (from your account dashboard)",
        validateInput: (v) =>
          /^\d+$/.test(v.trim()) ? null : "Enter a number.",
      });
      if (!remaining) {
        return;
      }
      const { daemonUrl, kind } = readConfig();
      postIngest(daemonUrl, {
        kind,
        host: "vscode-extension-manual",
        status_code: 200,
        pid: process.pid,
        sample: {
          provider: kind,
          tokens_remaining: parseInt(remaining.trim(), 10),
          requests_remaining: null,
          tokens_reset_at: null,
          requests_reset_at: null,
          raw_headers: { source: "manual" },
        },
      });
      vscode.window.showInformationMessage(
        `handoff: reported ${remaining} tokens remaining`
      );
    })
  );

  context.subscriptions.push({
    dispose: () => clearInterval(interval),
  });
}

export function deactivate(): void {
  /* noop */
}
