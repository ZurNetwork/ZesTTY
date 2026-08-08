// Minimal LSP stdio client — just enough protocol to drive the language
// server like an editor: Content-Length framing, requests with ids,
// notifications, and a waiter for server-pushed notifications
// (publishDiagnostics). No dependency on vscode-languageclient: the
// harness must not share plumbing with the thing it measures.

import { spawn } from "node:child_process";

export class LspClient {
  constructor(serverPath, { cwd }) {
    this.proc = spawn(process.execPath, [serverPath, "--stdio"], {
      cwd,
      stdio: ["pipe", "pipe", "inherit"],
    });
    this.nextId = 1;
    this.pending = new Map(); // id → resolve
    this.notificationWaiters = []; // { method, predicate, resolve }
    this.buffer = Buffer.alloc(0);
    this.proc.stdout.on("data", (chunk) => this.#onData(chunk));
  }

  #onData(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    for (;;) {
      const headerEnd = this.buffer.indexOf("\r\n\r\n");
      if (headerEnd === -1) return;
      const header = this.buffer.subarray(0, headerEnd).toString();
      const length = Number(/Content-Length: (\d+)/i.exec(header)?.[1]);
      const bodyStart = headerEnd + 4;
      if (this.buffer.length < bodyStart + length) return;
      const body = this.buffer.subarray(bodyStart, bodyStart + length);
      this.buffer = this.buffer.subarray(bodyStart + length);
      this.#dispatch(JSON.parse(body.toString()));
    }
  }

  #dispatch(msg) {
    if (msg.id !== undefined && this.pending.has(msg.id)) {
      this.pending.get(msg.id)(msg);
      this.pending.delete(msg.id);
      return;
    }
    if (msg.method) {
      const i = this.notificationWaiters.findIndex(
        (w) =>
          w.method === msg.method && (!w.predicate || w.predicate(msg.params)),
      );
      if (i !== -1)
        this.notificationWaiters.splice(i, 1)[0].resolve(msg.params);
    }
  }

  #send(msg) {
    const body = JSON.stringify({ jsonrpc: "2.0", ...msg });
    this.proc.stdin.write(
      `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`,
    );
  }

  request(method, params) {
    const id = this.nextId++;
    return new Promise((resolve) => {
      this.pending.set(id, resolve);
      this.#send({ id, method, params });
    });
  }

  notify(method, params) {
    this.#send({ method, params });
  }

  /** Resolves with the params of the next matching server notification. */
  waitForNotification(method, predicate) {
    return new Promise((resolve) => {
      this.notificationWaiters.push({ method, predicate, resolve });
    });
  }

  async initialize(rootDir) {
    const { pathToFileURL } = await import("node:url");
    await this.request("initialize", {
      processId: process.pid,
      capabilities: {},
      workspaceFolders: [{ uri: pathToFileURL(rootDir).href, name: "bench" }],
    });
    this.notify("initialized", {});
  }

  async shutdown() {
    await this.request("shutdown", null);
    this.notify("exit", null);
    const killTimer = setTimeout(() => this.proc.kill("SIGKILL"), 2000);
    await new Promise((resolve) => this.proc.once("exit", resolve));
    clearTimeout(killTimer);
  }
}
