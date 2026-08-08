// LS keystroke latency: didChange → publishDiagnostics round-trip (the
// server recompiles on every content change under Full sync — exactly
// what an editor keystroke costs today), plus completion request latency.

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { LspClient } from "../lsp-client.js";
import { now, ms, summarize } from "../stats.js";
import { generateModule } from "../gen.js";

export async function run(
  { repoRoot, workDir, sections, keystrokes, completions },
  log,
) {
  const dir = join(workDir, "ls");
  mkdirSync(dir, { recursive: true });
  let text = generateModule(sections);
  const file = join(dir, "doc.zts");
  writeFileSync(file, text);
  const uri = pathToFileURL(file).href;

  const client = new LspClient(
    join(repoRoot, "packages/language-server/server.js"),
    { cwd: dir },
  );
  await client.initialize(dir);

  const opened = client.waitForNotification(
    "textDocument/publishDiagnostics",
    (p) => p.uri === uri,
  );
  client.notify("textDocument/didOpen", {
    textDocument: { uri, languageId: "zts", version: 1, text },
  });
  await opened;
  log(`ls: server up, document open (${text.split("\n").length} lines)`);

  const keystrokeSamples = [];
  for (let i = 0; i < keystrokes; i++) {
    text += `const __bench${i} = ${i};\n`;
    const arrived = client.waitForNotification(
      "textDocument/publishDiagnostics",
      (p) => p.uri === uri,
    );
    const t0 = now();
    client.notify("textDocument/didChange", {
      textDocument: { uri, version: 2 + i },
      contentChanges: [{ text }],
    });
    await arrived;
    keystrokeSamples.push(ms(t0, now()));
  }

  // Completion after a member dot — the position editors actually ask at.
  const dotOffset = text.indexOf("Shape0.fmt") + "Shape0.".length;
  const before = text.slice(0, dotOffset);
  const line = before.split("\n").length - 1;
  const character = dotOffset - (before.lastIndexOf("\n") + 1);
  const completionSamples = [];
  for (let i = 0; i < completions; i++) {
    const t0 = now();
    await client.request("textDocument/completion", {
      textDocument: { uri },
      position: { line, character },
    });
    completionSamples.push(ms(t0, now()));
  }

  await client.shutdown();
  return {
    "ls.keystroke_diagnostics": summarize(keystrokeSamples),
    "ls.completion": summarize(completionSamples),
  };
}
