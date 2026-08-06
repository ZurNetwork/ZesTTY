import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { ztsCheck } from "@zestty/check";

const FIXTURES = join(dirname(fileURLToPath(import.meta.url)), "fixtures");

function run(name) {
  const lines = [];
  const code = ztsCheck(join(FIXTURES, name), { log: (l) => lines.push(l) });
  return { code, out: lines.join("\n") };
}

test("clean project passes, twins are cleaned up", () => {
  const { code, out } = run("clean");
  assert.equal(code, 0, out);
  assert.match(out, /zts-check: clean/);
  assert.ok(!existsSync(join(FIXTURES, "clean/shape.ts")), "twin not removed");
  assert.ok(!existsSync(join(FIXTURES, "clean/main.ts")), "twin not removed");
});

test("missing match arm fails with a diagnostic on the .zts source", () => {
  const { code, out } = run("missing-arm");
  assert.equal(code, 1);
  assert.match(out, /missing\.zts\(\d+,\d+\): error TS2345/);
  // The mapped position must be the match, not generated scaffolding.
  const line = Number(out.match(/missing\.zts\((\d+),/)[1]);
  const src = readFileSync(
    join(FIXTURES, "missing-arm/missing.zts"),
    "utf8",
  ).split("\n");
  assert.match(src[line - 1], /match/);
  assert.ok(!existsSync(join(FIXTURES, "missing-arm/missing.ts")));
});

test("svelte lang=zts script is checked and mapped into the component", () => {
  const { code, out } = run("svelte");
  assert.equal(code, 1);
  assert.match(out, /Widget\.svelte\(\d+,\d+\): error/);
  assert.match(out, /never/);
  const line = Number(out.match(/Widget\.svelte\((\d+),/)[1]);
  const src = readFileSync(
    join(FIXTURES, "svelte/Widget.svelte"),
    "utf8",
  ).split("\n");
  assert.match(src[line - 1], /match/);
  assert.ok(!existsSync(join(FIXTURES, "svelte/Widget.svelte-script.ts")));
});

test("stale committed twin is reported and preserved", () => {
  const { code, out } = run("stale-twin");
  assert.equal(code, 1);
  assert.match(out, /stale committed twin/);
  // The committed twin must NOT be deleted or overwritten.
  assert.equal(
    readFileSync(join(FIXTURES, "stale-twin/foo.ts"), "utf8"),
    "export const answer: number = 41;\n",
  );
});
