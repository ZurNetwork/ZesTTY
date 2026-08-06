// Regression tests for the Phase 3 review-gate findings.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { ztsCheck } from "@zestty/check";

const FIXTURES = join(dirname(fileURLToPath(import.meta.url)), "fixtures");

function run(name) {
  const lines = [];
  const code = ztsCheck(join(FIXTURES, name), { log: (l) => lines.push(l) });
  return { code, out: lines.join("\n") };
}

test("C1: a .zts outside tsconfig include is a loud error, not false-green", () => {
  const { code, out } = run("excluded");
  assert.equal(code, 1, out);
  assert.match(out, /not covered by the project tsconfig/);
  assert.match(out, /broken\.zts/);
});

test("H3: every svelte lang=zts block is checked, not just the first", () => {
  const { code, out } = run("two-scripts");
  assert.equal(code, 1, out);
  assert.match(out, /Two\.svelte\(\d+,\d+\): error/);
  assert.match(out, /never/);
  // The failing match lives in the SECOND block.
  const line = Number(out.match(/Two\.svelte\((\d+),/)[1]);
  assert.ok(line > 5, `diagnostic at line ${line}, expected the second block`);
});

test("C2/parity: extensionless cross-imports check clean (same naming as the LSP)", () => {
  const { code, out } = run("cross-import");
  assert.equal(code, 0, out);
  assert.ok(!existsSync(join(FIXTURES, "cross-import/dep.ts")));
  assert.ok(!existsSync(join(FIXTURES, "cross-import/use.ts")));
});

test("low: empty project exits 0 with a note, not tsc's help screen", () => {
  const { code, out } = run("empty-project");
  assert.equal(code, 0, out);
  assert.match(out, /nothing to check/);
  assert.doesNotMatch(out, /COMMON COMMANDS/);
});

test("low: nonexistent root is a friendly error", () => {
  const lines = [];
  const code = ztsCheck(join(FIXTURES, "does-not-exist"), {
    log: (l) => lines.push(l),
  });
  assert.equal(code, 2);
  assert.match(lines.join("\n"), /no such directory/);
});

test("M9: a CRLF committed twin is not reported stale", () => {
  const dir = mkdtempSync(join(tmpdir(), "zts-crlf-"));
  try {
    writeFileSync(
      join(dir, "x.zts"),
      "export const n: number = if (true as boolean) { 1 } else { 2 };\n",
    );
    // First run with --keep to obtain the twin, then CRLF-ify it.
    let code = ztsCheck(dir, { keep: true, log: () => {} });
    assert.equal(code, 0);
    const twin = readFileSync(join(dir, "x.ts"), "utf8");
    writeFileSync(join(dir, "x.ts"), twin.replace(/\n/g, "\r\n"));
    rmSync(join(dir, ".zts-check"), { recursive: true, force: true });
    const lines = [];
    code = ztsCheck(dir, { log: (l) => lines.push(l) });
    assert.equal(code, 0, lines.join("\n"));
    assert.doesNotMatch(lines.join("\n"), /stale committed twin/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("H4: twins leaked by a dead run are recovered, not adopted", () => {
  const dir = mkdtempSync(join(tmpdir(), "zts-leak-"));
  try {
    writeFileSync(
      join(dir, "a.zts"),
      "export const n: number = if (true as boolean) { 1 } else { 2 };\n",
    );
    // Simulate an interrupted run: twin on disk + manifest present.
    let code = ztsCheck(dir, { keep: true, log: () => {} });
    assert.equal(code, 0);
    assert.ok(existsSync(join(dir, "a.ts")), "twin should exist after --keep");
    assert.ok(
      existsSync(join(dir, ".zts-check/created.json")),
      "manifest should persist with --keep",
    );

    // Next run must delete the leak (manifest says WE made it), then
    // regenerate + clean up normally: no stale-twin error, exit 0.
    const lines = [];
    code = ztsCheck(dir, { log: (l) => lines.push(l) });
    assert.equal(code, 0, lines.join("\n"));
    assert.match(lines.join("\n"), /leaked by an interrupted run/);
    assert.ok(!existsSync(join(dir, "a.ts")), "leak must be cleaned");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("H6: .ztsx without a tsconfig gets --jsx (no TS17004)", () => {
  const dir = mkdtempSync(join(tmpdir(), "zts-tsx-"));
  try {
    writeFileSync(
      join(dir, "el.ztsx"),
      "export const el = <div>{if (true as boolean) { 1 } else { 2 }}</div>;\n",
    );
    const lines = [];
    ztsCheck(dir, { log: (l) => lines.push(l) });
    // A bare temp dir has no React types, so JSX.IntrinsicElements errors
    // are legitimate — but "Cannot use JSX unless --jsx is provided"
    // (TS17004) means the flag is missing, which was the bug.
    assert.doesNotMatch(lines.join("\n"), /TS17004/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
