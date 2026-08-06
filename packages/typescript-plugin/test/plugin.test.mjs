// typescript-zestty-plugin (issue #45): definitions that land in a
// committed twin are remapped to the sibling .zts — through the twin's
// sourcemap when present, by whole-word symbol search when not.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { generateTwins } from "@zestty/check";
import init from "../index.js";

const ZTS_SOURCE = `enum Shape {
  Circle { r: number },
}
export const answer: number = if (true as boolean) { 1 } else { 2 };
`;

/** A twins-mode consumer dir: a.zts plus its generated a.ts + a.ts.map. */
function twinDir() {
  const dir = mkdtempSync(join(tmpdir(), "zts-plugin-"));
  writeFileSync(join(dir, "a.zts"), ZTS_SOURCE);
  const code = generateTwins(dir, { log: () => {} });
  assert.equal(code, 0);
  return dir;
}

/**
 * The plugin proxy over a stub language service whose definition queries
 * all answer with `defs` — the tsserver behavior under test is only the
 * remapping layer on top.
 */
function pluginProxy(defs) {
  const stub = {
    getDefinitionAndBoundSpan: () => ({
      textSpan: { start: 0, length: 1 },
      definitions: defs,
    }),
    getDefinitionAtPosition: () => defs,
    getTypeDefinitionAtPosition: () => defs,
    getImplementationAtPosition: () => undefined,
    getQuickInfoAtPosition: () => "untouched-passthrough",
  };
  return init().create({ languageService: stub });
}

function defAt(dir, twinText, name) {
  return {
    fileName: join(dir, "a.ts"),
    name,
    textSpan: { start: twinText.indexOf(name), length: name.length },
    contextSpan: { start: 0, length: twinText.length },
    kind: "const",
    containerName: "",
  };
}

test("remaps a twin definition to the .zts through the sourcemap", () => {
  const dir = twinDir();
  try {
    const twinText = readFileSync(join(dir, "a.ts"), "utf8");
    const proxy = pluginProxy([defAt(dir, twinText, "answer")]);

    const res = proxy.getDefinitionAndBoundSpan(join(dir, "consumer.ts"), 0);
    const [def] = res.definitions;
    assert.equal(def.fileName, join(dir, "a.zts"));
    assert.equal(
      ZTS_SOURCE.slice(
        def.textSpan.start,
        def.textSpan.start + def.textSpan.length,
      ),
      "answer",
    );
    assert.equal(def.contextSpan, undefined);
    // The bound span (in the consumer) is not touched.
    assert.deepEqual(res.textSpan, { start: 0, length: 1 });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("enum factory definitions land on the .zts enum, not the lowering", () => {
  const dir = twinDir();
  try {
    const twinText = readFileSync(join(dir, "a.ts"), "utf8");
    const proxy = pluginProxy([defAt(dir, twinText, "Circle")]);

    const [def] = proxy.getDefinitionAtPosition(join(dir, "consumer.ts"), 0);
    assert.equal(def.fileName, join(dir, "a.zts"));
    assert.equal(
      ZTS_SOURCE.slice(
        def.textSpan.start,
        def.textSpan.start + def.textSpan.length,
      ),
      "Circle",
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("falls back to whole-word symbol search when the map is missing", () => {
  const dir = twinDir();
  try {
    unlinkSync(join(dir, "a.ts.map"));
    const twinText = readFileSync(join(dir, "a.ts"), "utf8");
    const proxy = pluginProxy([defAt(dir, twinText, "answer")]);

    const [def] = proxy.getDefinitionAtPosition(join(dir, "consumer.ts"), 0);
    assert.equal(def.fileName, join(dir, "a.zts"));
    assert.equal(
      ZTS_SOURCE.slice(
        def.textSpan.start,
        def.textSpan.start + def.textSpan.length,
      ),
      "answer",
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("leaves non-generated .ts definitions alone", () => {
  const dir = mkdtempSync(join(tmpdir(), "zts-plugin-plain-"));
  try {
    writeFileSync(join(dir, "a.ts"), "export const answer = 1;\n");
    const def = {
      fileName: join(dir, "a.ts"),
      name: "answer",
      textSpan: { start: 13, length: 6 },
    };
    const proxy = pluginProxy([def]);
    const [out] = proxy.getDefinitionAtPosition(join(dir, "consumer.ts"), 0);
    assert.deepEqual(out, def);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("leaves an orphan twin (missing .zts source) alone", () => {
  const dir = twinDir();
  try {
    unlinkSync(join(dir, "a.zts"));
    const twinText = readFileSync(join(dir, "a.ts"), "utf8");
    const original = defAt(dir, twinText, "answer");
    const proxy = pluginProxy([original]);
    const [out] = proxy.getDefinitionAtPosition(join(dir, "consumer.ts"), 0);
    assert.deepEqual(out, original);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("passes every other language-service method through", () => {
  const proxy = pluginProxy([]);
  assert.equal(proxy.getQuickInfoAtPosition(), "untouched-passthrough");
});
