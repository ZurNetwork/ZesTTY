import { test } from "node:test";
import assert from "node:assert/strict";
import { SourceMapConsumer } from "source-map-js";
import zts from "@zestty/vite-plugin";

const FIXTURE = `type Shape =
  | { kind: "Circle"; radius: number }
  | { kind: "Square"; side: number };
declare const shape: Shape;
export const area = match (shape) {
  Circle { radius } => 3.14 * radius ** 2,
  Square { side } => side ** 2,
};
`;

function pluginInstance() {
  const plugin = zts();
  plugin.configResolved({ isProduction: false });
  return plugin;
}

const ctx = {
  error(msg) {
    throw new Error(String(msg));
  },
};

test("transforms .zts to plain JS with a composed sourcemap", async () => {
  const plugin = pluginInstance();
  const out = await plugin.transform.call(ctx, FIXTURE, "/src/shape.zts");

  // Types erased end-to-end, lowering applied.
  assert.match(out.code, /__ztsAbsurd/);
  assert.doesNotMatch(out.code, /kind:\s*"Circle";/);
  assert.doesNotMatch(out.code, /: number/);

  // Sourcemap proof (Phase 2 exit criterion, headless form): a position
  // inside the generated arm body maps back to the .zts arm source line.
  const lines = out.code.split("\n");
  const genLine = lines.findIndex((l) => l.includes("3.14")) + 1;
  const genCol = lines[genLine - 1].indexOf("3.14");
  const consumer = new SourceMapConsumer(out.map);
  const orig = consumer.originalPositionFor({ line: genLine, column: genCol });
  assert.ok(
    orig.source && orig.source.includes("shape.zts"),
    `source: ${orig.source}`,
  );
  const srcLines = FIXTURE.split("\n");
  assert.match(
    srcLines[orig.line - 1],
    /Circle \{ radius \}/,
    `mapped to .zts line ${orig.line}: ${srcLines[orig.line - 1]}`,
  );
});

test("ignores non-zts modules", async () => {
  const plugin = pluginInstance();
  assert.equal(
    await plugin.transform.call(ctx, "const a = 1;", "/src/a.ts"),
    null,
  );
});

test("handles vite query-suffixed ids", async () => {
  const plugin = pluginInstance();
  const out = await plugin.transform.call(
    ctx,
    FIXTURE,
    "/src/shape.zts?import",
  );
  assert.match(out.code, /__ztsAbsurd/);
});

test("diagnostics surface through this.error", async () => {
  const plugin = pluginInstance();
  await assert.rejects(
    plugin.transform.call(
      ctx,
      "const r = match (t) { K { v: bad } => v };",
      "/src/bad.zts",
    ),
    /shorthand identifiers/,
  );
});

test("survives a non-throwing this.error host", async () => {
  // Rollup's this.error throws, but not every plugin container does; the
  // diagnostic must come back either way — never a TypeError on stage1.
  const plugin = pluginInstance();
  const seen = [];
  const softCtx = {
    error(msg) {
      seen.push(String(msg));
      return undefined;
    },
  };
  const out = await plugin.transform.call(
    softCtx,
    "const r = match (t) { K { v: bad } => v };",
    "/src/bad.zts",
  );
  assert.equal(out, undefined);
  assert.match(seen[0], /shorthand identifiers/);
});

test("strips fragment suffixes from ids", async () => {
  const plugin = pluginInstance();
  const out = await plugin.transform.call(ctx, FIXTURE, "/src/shape.zts#frag");
  assert.match(out.code, /__ztsAbsurd/);
});
