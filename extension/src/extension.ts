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

interface CompanionConfig {
  daemonUrl: string;
  kind: string;
  heartbeatSeconds: number;
  detectExtensions: boolean;
}

interface ExtensionMatcher {
  kind: string;
  needles: string[];
}

const KNOWN_AI_EXTENSIONS: ExtensionMatcher[] = [
  {
    kind: "codex",
    needles: ["openai.chatgpt", "openai.codex", ".codex", "codex"],
  },
  {
    kind: "claude",
    needles: ["anthropic.claude-code", "claude-code", "claude code"],
  },
  {
    kind: "copilot",
    needles: ["github.copilot", "github.copilot-chat", "copilot"],
  },
  {
    kind: "antigravity",
    needles: ["antigravity"],
  },
  {
    kind: "gemini",
    needles: ["google.geminicodeassist", "geminicodeassist", "gemini code assist"],
  },
];

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

function readConfig(): CompanionConfig {
  const cfg = vscode.workspace.getConfiguration("handoff");
  return {
    daemonUrl: cfg.get<string>("daemonUrl", "http://127.0.0.1:7879"),
    kind: cfg.get<string>("kind", "auto"),
    heartbeatSeconds: cfg.get<number>("heartbeatSeconds", 30),
    detectExtensions: cfg.get<boolean>("detectExtensions", true),
  };
}

function extensionSearchText(extension: vscode.Extension<unknown>): string {
  const pkg = extension.packageJSON as Record<string, unknown>;
  return [
    extension.id,
    pkg.name,
    pkg.displayName,
    pkg.description,
    pkg.publisher,
  ]
    .filter((part): part is string => typeof part === "string")
    .join(" ")
    .toLowerCase();
}

function detectActiveExtensionKinds(): string[] {
  const kinds = new Set<string>();
  for (const extension of vscode.extensions.all) {
    if (!extension.isActive) {
      continue;
    }
    const haystack = extensionSearchText(extension);
    for (const matcher of KNOWN_AI_EXTENSIONS) {
      if (matcher.needles.some((needle) => haystack.includes(needle))) {
        kinds.add(matcher.kind);
      }
    }
  }
  return [...kinds].sort();
}

function reportedKinds(config: CompanionConfig): string[] {
  if (config.kind !== "auto") {
    return [config.kind];
  }
  if (!config.detectExtensions) {
    return ["vscode"];
  }
  const detected = detectActiveExtensionKinds();
  return detected.length > 0 ? detected : ["vscode"];
}

export function activate(context: vscode.ExtensionContext): void {
  const out = vscode.window.createOutputChannel("handoff");
  out.appendLine(`handoff companion activated (pid=${process.pid})`);

  const sendHeartbeat = (statusCode: number = 200): void => {
    const config = readConfig();
    for (const kind of reportedKinds(config)) {
      postIngest(config.daemonUrl, {
        kind,
        host: `vscode-extension:${kind}`,
        status_code: statusCode,
        pid: process.pid,
        sample: null,
      });
    }
  };

  // Initial heartbeat + interval
  sendHeartbeat();
  let interval = setInterval(
    sendHeartbeat,
    Math.max(5, readConfig().heartbeatSeconds) * 1000
  );

  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (
        e.affectsConfiguration("handoff.heartbeatSeconds") ||
        e.affectsConfiguration("handoff.kind") ||
        e.affectsConfiguration("handoff.detectExtensions")
      ) {
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
      const config = readConfig();
      const kinds = reportedKinds(config);
      const kind =
        kinds.length === 1
          ? kinds[0]
          : await vscode.window.showQuickPick(kinds, {
              placeHolder: "Which agent should receive this usage sample?",
            });
      if (!kind) {
        return;
      }
      postIngest(config.daemonUrl, {
        kind,
        host: `vscode-extension-manual:${kind}`,
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
