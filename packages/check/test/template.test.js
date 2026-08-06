// Svelte TEMPLATE bindings against zts script members (shadow svelte-check).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { ztsCheck } from "@zestty/check";
import {
  transformComponent,
  remapComponentPosition,
} from "@zestty/check/shadow";

const FIXTURES = join(dirname(fileURLToPath(import.meta.url)), "fixtures");

function run(name) {
  const lines = [];
  const code = ztsCheck(join(FIXTURES, name), { log: (l) => lines.push(l) });
  return { code, out: lines.join("\n") };
}

test("template binding error maps to the exact original template line", () => {
  const { code, out } = run("template-error");
  assert.equal(code, 1, out);
  assert.match(out, /Price\.svelte\(\d+,\d+\): error \(svelte-check\)/);
  const line = Number(out.match(/Price\.svelte\((\d+),/)[1]);
  const src = readFileSync(
    join(FIXTURES, "template-error/Price.svelte"),
    "utf8",
  ).split("\n");
  // The offending expression is BELOW the script — the delta path.
  assert.match(src[line - 1], /label\.toFixed/, `mapped to: ${src[line - 1]}`);
});

test("clean component with template usage passes", () => {
  const { code, out } = run("template-clean");
  assert.equal(code, 0, out);
});

test("--no-svelte skips template checking (script twins only)", () => {
  const lines = [];
  const code = ztsCheck(join(FIXTURES, "template-error"), {
    svelte: false,
    log: (l) => lines.push(l),
  });
  // Script itself is fine; the template bug is invisible in this mode.
  assert.equal(code, 0, lines.join("\n"));
});

test("transformComponent rewrites lang and preserves other attributes", () => {
  const src = `<script context="module" lang='zts'>\nconst n = not false;\n</script>\n<p>hi</p>\n`;
  const t = transformComponent(src, "/tmp/X.svelte");
  assert.ok(t);
  assert.match(t.code, /<script context="module" lang="ts">/);
  assert.match(t.code, /!false/);
  assert.match(t.code, /<p>hi<\/p>/);
});

test("remapComponentPosition: above, inside, and below the script", () => {
  const src = `<p>above</p>
<script lang="zts">
  const n: number = if (true as boolean) { 1 } else { 2 };
</script>
<p>below</p>
`;
  const t = transformComponent(src, "/tmp/Y.svelte");
  const compLines = t.blocks[0].compLines;
  const origLines = t.blocks[0].origLines;
  const delta = compLines - origLines;
  // Above: identity.
  assert.deepEqual(remapComponentPosition(t.blocks, 1, 4), {
    line: 1,
    column: 4,
  });
  // Below: shifted back by the delta. `<p>below</p>` is original line 5.
  assert.deepEqual(remapComponentPosition(t.blocks, 5 + delta, 4), {
    line: 5,
    column: 4,
  });
  // Inside: maps into the script (line 3 carries the const).
  const inside = remapComponentPosition(t.blocks, 3 + 1, 11);
  assert.equal(inside.line, 3);
});
