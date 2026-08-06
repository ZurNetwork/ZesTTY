// Issue #35: --twins --watch regenerates continuously without self-triggering.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
  existsSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { watchTwins } from "@zestty/check";

const CORE_PKG = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "core",
);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function until(fn, timeoutMs = 5000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (fn()) return true;
    await sleep(50);
  }
  return fn();
}

test("watchTwins: initial run, change-driven regen, exec hook, no self-trigger", async () => {
  const dir = mkdtempSync(join(tmpdir(), "zts-watch-"));
  const logs = [];
  let handle = null;
  try {
    mkdirSync(join(dir, "node_modules", "@zestty"), { recursive: true });
    symlinkSync(CORE_PKG, join(dir, "node_modules", "@zestty", "core"));
    writeFileSync(join(dir, "a.zts"), "export const n: number = 1;\n");

    handle = watchTwins(dir, {
      log: (l) => logs.push(l),
      exec: `node -e "require('fs').appendFileSync('${join(dir, "hook.log")}', 'x')"`,
      debounceMs: 50,
    });

    // Initial pass wrote the twin and ran the hook once.
    assert.ok(existsSync(join(dir, "a.ts")), "initial twin");
    assert.ok(
      await until(() => existsSync(join(dir, "hook.log"))),
      "initial exec hook",
    );
    const hooksAfterStart = readFileSync(join(dir, "hook.log"), "utf8").length;

    // Source change → twin regenerated, hook re-run.
    writeFileSync(join(dir, "a.zts"), "export const n: number = 2;\n");
    assert.ok(
      await until(() => readFileSync(join(dir, "a.ts"), "utf8").includes("2")),
      "twin regenerated after source change",
    );
    assert.ok(
      await until(
        () =>
          readFileSync(join(dir, "hook.log"), "utf8").length > hooksAfterStart,
      ),
      "exec hook re-ran",
    );

    // The twin write itself (and hook output) must not have queued more
    // regenerations: after the system settles, hook count is stable.
    await sleep(400);
    const settled = readFileSync(join(dir, "hook.log"), "utf8").length;
    await sleep(400);
    assert.equal(
      readFileSync(join(dir, "hook.log"), "utf8").length,
      settled,
      "no self-triggered regeneration loop",
    );
  } finally {
    handle?.close();
    rmSync(dir, { recursive: true, force: true });
  }
});
