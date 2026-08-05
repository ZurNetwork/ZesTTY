// VS Code extension entry: starts @zestty/language-server for zts files.
// CommonJS on purpose — the VS Code extension host loads CJS.
const path = require("node:path");
const fs = require("node:fs");
const { workspace } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

/** @type {import("vscode-languageclient/node").LanguageClient | undefined} */
let client;

function resolveServer(context) {
  const configured = workspace
    .getConfiguration("zestty")
    .get("languageServer.path");
  if (configured) {
    if (fs.existsSync(configured)) return configured;
    console.warn(
      `zestty: configured languageServer.path not found: ${configured}`,
    );
  }

  const candidates = [];
  const root = workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (root) {
    // Consumer projects with @zestty/language-server installed. (The bare
    // repo-layout candidate was deliberately dropped: executing a path a
    // workspace merely CONTAINS — no install step — is a wider surface
    // than the norm; repo devs use the setting or the extension-relative
    // path below.)
    candidates.push(
      path.join(
        root,
        "node_modules",
        "@zestty",
        "language-server",
        "server.js",
      ),
    );
  }
  // Relative to the extension (repo-dev install from editors/vscode).
  candidates.push(
    context.asAbsolutePath(
      path.join("..", "..", "packages", "language-server", "server.js"),
    ),
  );
  return candidates.find((c) => fs.existsSync(c));
}

exports.activate = function activate(context) {
  const serverPath = resolveServer(context);
  if (!serverPath) {
    console.warn(
      "zestty: @zestty/language-server not found — syntax highlighting only. " +
        "Install it in the workspace or set zestty.languageServer.path.",
    );
    return;
  }

  const serverOptions = {
    run: {
      module: serverPath,
      transport: TransportKind.ipc,
    },
    debug: {
      module: serverPath,
      transport: TransportKind.ipc,
      options: { execArgv: ["--inspect=6009"] },
    },
  };

  client = new LanguageClient(
    "zestty",
    "ZesTTY Language Server",
    serverOptions,
    {
      documentSelector: [{ scheme: "file", language: "zts" }],
    },
  );
  context.subscriptions.push({ dispose: () => client?.stop() });
  client.start();
};

exports.deactivate = function deactivate() {
  return client?.stop();
};
