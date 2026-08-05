import { test } from "node:test";
import assert from "node:assert/strict";
import { compile } from "@zts/native";

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

test("deep nesting is a diagnostic, not a crash", () => {
  const n = 5000;
  const src = `const a = ${"(".repeat(n)}1${")".repeat(n)};`;
  assert.throws(
    () => compile(src, "deep.zts"),
    (err) => err.message.includes("nesting exceeds"),
  );
});
