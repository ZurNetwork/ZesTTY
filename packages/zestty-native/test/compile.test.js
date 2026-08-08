import { test } from "node:test";
import assert from "node:assert/strict";
import { compile, format } from "@zestty/native";

const SHAPE = `
type Shape =
  | { kind: "Circle"; radius: number }
  | { kind: "Square"; side: number };
declare const shape: Shape;
const area = match (shape) {
  Circle { radius } => radius * 2,
  Square { side } => side ** 2,
};
`;

test("compiles a match expression", () => {
  const { code, map } = compile(SHAPE, "shape.zts");
  assert.match(code, /__ztsAbsurd/);
  assert.match(code, /__m\.kind/);
  assert.ok(JSON.parse(map).sources.some((s) => s.includes("shape.zts")));
});

test("vanilla TS passes through", () => {
  const { code } = compile("const x: number = 1;\n", "x.zts");
  assert.match(code, /const x: number = 1;/);
  assert.doesNotMatch(code, /__ztsAbsurd/);
});

test("diagnostics arrive as the error message", () => {
  assert.throws(
    () => compile("const r = match (t) { K { v: bad } => v };", "bad.zts"),
    (err) => err.message.includes("shorthand identifiers"),
  );
});

test("tsx option gates JSX", () => {
  const src = "export const el = <div>hi</div>;\n";
  const { code } = compile(src, "el.ztsx", { tsx: true });
  assert.match(code, /<div>/);
  assert.throws(() => compile(src, "el.zts"));
});

// One fixture per language feature, compiled through the .node binding.
// Guards against the binding silently lagging the compiler (issue #2): a
// stale prebuilt binding predating a feature fails HERE, not as a confusing
// parse error inside a consumer's svelte-check.

test("feature: enums-with-data compile through the binding", () => {
  const { code } = compile(
    "enum Shape { Circle { radius: number }, Square { side: number } }\nvoid Shape;\n",
    "feature_enum.zts",
  );
  assert.match(code, /type Shape =/);
  assert.match(code, /Circle: \(radius: number\)/);
  assert.doesNotMatch(code, /enum /);
});

test("feature: expression if compiles through the binding", () => {
  const { code } = compile(
    "declare const b: boolean;\nconst a = if (b) { 1 } else { 2 };\nvoid a;\n",
    "feature_if.zts",
  );
  assert.match(code, /b \? 1 : 2/);
});

test("feature: match arm block bodies compile through the binding", () => {
  const { code } = compile(
    'declare const t: { kind: "K"; v: number };\nconst r = match (t) { K { v } => { const d = v * 2; d } };\nvoid r;\n',
    "feature_arm_block.zts",
  );
  assert.match(code, /const d = v \* 2;/);
});

test("deep nesting is a diagnostic, not a crash", () => {
  const n = 5000;
  const src = `const a = ${"(".repeat(n)}1${")".repeat(n)};`;
  assert.throws(
    () => compile(src, "deep.zts"),
    (err) => err.message.includes("nesting exceeds"),
  );
});

test("zts-fmt: format() canonicalizes and is null when already formatted", () => {
  const messy = "enum   E{A{x:number}}\n";
  const once = format(messy, "t.zts");
  assert.match(once, /enum E \{\n  A \{ x: number \},\n\}/);
  assert.equal(format(once, "t.zts"), null);
});

test("zts-fmt: explicit options shape the output (issue #70)", () => {
  const src = 'const x = "a";\nfunction f() {\nreturn x;\n}\n';
  const out = format(src, "t.zts", { useTabs: true, singleQuote: true });
  assert.match(out, /const x = 'a';/);
  assert.match(out, /\treturn x;/);
});

test("zts-fmt: imports keep source order by default (issue #70)", () => {
  const src =
    'import { z, a } from "./b";\nimport "./a";\nexport const k = a + z;\n';
  assert.equal(format(src, "t.zts"), null);
  const sorted = format(src, "t.zts", { sortImports: true });
  assert.match(sorted, /\{ a, z \}/);
});

test("zts-fmt: zts-fmt.json is discovered upward and fails closed (issue #70)", async () => {
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } =
    await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const root = mkdtempSync(join(tmpdir(), "zts-fmt-"));
  try {
    const nested = join(root, "src");
    mkdirSync(nested);
    writeFileSync(join(root, "zts-fmt.json"), '{ "singleQuote": true }');
    const out = format('const x = "a";\n', join(nested, "t.zts"));
    assert.match(out, /const x = 'a';/);
    writeFileSync(join(root, "zts-fmt.json"), '{ "semi": false }');
    assert.throws(
      () => format('const x = "a";\n', join(nested, "t.zts")),
      /unknown option "semi"/,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
