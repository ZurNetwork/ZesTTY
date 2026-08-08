// The harness must always RUN — smoke mode end-to-end through all three
// suites, then sanity-check the results file. Numbers are not asserted:
// this is a works-everywhere gate, not a perf gate.

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const benchDir = dirname(dirname(fileURLToPath(import.meta.url)));
const outPath = join(benchDir, ".work", "smoke-results.json");

test("bench harness runs end-to-end in smoke mode", () => {
  rmSync(outPath, { force: true });
  const r = spawnSync(
    process.execPath,
    [join(benchDir, "run.js"), "--smoke", "--out", outPath],
    { encoding: "utf8", timeout: 120_000 },
  );
  assert.equal(r.status, 0, `run.js failed:\n${r.stdout}\n${r.stderr}`);

  const { meta, results } = JSON.parse(readFileSync(outPath, "utf8"));
  assert.equal(meta.smoke, true);
  for (const key of [
    "compile.fixtures_corpus",
    "compile.synthetic_large",
    "compile.small_file",
    "ls.keystroke_diagnostics",
    "ls.completion",
    "check.wallclock",
  ]) {
    const s = results[key];
    assert.ok(s, `missing ${key}`);
    assert.ok(s.mean > 0 && s.n > 0, `${key} has no samples`);
  }
});
