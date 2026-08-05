import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ZtsProject } from "@zestty/language-server";

const MISSING_ARM = `type T = { kind: "K"; v: number } | { kind: "L"; w: number };
declare const t: T;

export const r = match (t) {
  K { v } => v,
};
`;

const CLEAN = `enum Shape {
  Circle { radius: number },
  Square { side: number },
}

const c = Shape.Circle(2);

export const area = match (c) {
  Circle { radius } => radius * 2,
  Square { side } => side ** 2,
};
`;

function tempProject() {
  const dir = mkdtempSync(join(tmpdir(), "zts-lsp-"));
  return { dir, cleanup: () => rmSync(dir, { recursive: true, force: true }) };
}

test("missing arm produces a diagnostic on the original match line", () => {
  const { dir, cleanup } = tempProject();
  try {
    const project = new ZtsProject(dir);
    const path = join(dir, "missing.zts");
    project.upsert(path, MISSING_ARM);
    const diags = project.diagnostics(path);
    assert.ok(diags.length >= 1, "expected at least one diagnostic");
    const d = diags.find((d) => /never/.test(d.message));
    assert.ok(d, `no never-diagnostic in: ${JSON.stringify(diags)}`);
    // Line 3 (0-based) is `export const r = match (t) {`.
    assert.equal(d.range.start.line, 3);
    assert.match(MISSING_ARM.split("\n")[d.range.start.line], /match/);
  } finally {
    cleanup();
  }
});

test("clean document has no diagnostics", () => {
  const { dir, cleanup } = tempProject();
  try {
    const project = new ZtsProject(dir);
    const path = join(dir, "clean.zts");
    project.upsert(path, CLEAN);
    assert.deepEqual(project.diagnostics(path), []);
  } finally {
    cleanup();
  }
});

test("parse errors surface as diagnostics with a mapped position", () => {
  const { dir, cleanup } = tempProject();
  try {
    const project = new ZtsProject(dir);
    const path = join(dir, "bad.zts");
    project.upsert(path, "const r = match (t) { K { v: bad } => v };\n");
    const diags = project.diagnostics(path);
    assert.equal(diags.length, 1);
    assert.match(diags[0].message, /shorthand identifiers/);
    assert.equal(diags[0].range.start.line, 0);
  } finally {
    cleanup();
  }
});

test("hover over an enum factory shows its arrow type", () => {
  const { dir, cleanup } = tempProject();
  try {
    const project = new ZtsProject(dir);
    const path = join(dir, "hover.zts");
    project.upsert(path, CLEAN);
    // Position of `Circle` in `Shape.Circle(2)` (0-based line 5).
    const line = CLEAN.split("\n")[5];
    const character = line.indexOf("Circle");
    const hover = project.hover(path, { line: 5, character });
    assert.ok(hover, "expected hover info");
    assert.match(hover.contents.value, /radius: number/);
  } finally {
    cleanup();
  }
});

test("go-to-definition from factory call lands on the enum declaration", () => {
  const { dir, cleanup } = tempProject();
  try {
    const project = new ZtsProject(dir);
    const path = join(dir, "def.zts");
    project.upsert(path, CLEAN);
    const line = CLEAN.split("\n")[5];
    const character = line.indexOf("Circle");
    const defs = project.definitions(path, { line: 5, character });
    assert.ok(defs.length >= 1, "expected a definition");
    assert.equal(defs[0].path, path);
    // The enum lowers to a hoisted const whose span is the original enum;
    // the definition must land inside the enum declaration (lines 0-3).
    assert.ok(
      defs[0].range.start.line <= 3,
      `definition at line ${defs[0].range.start.line}, expected within the enum`,
    );
  } finally {
    cleanup();
  }
});
