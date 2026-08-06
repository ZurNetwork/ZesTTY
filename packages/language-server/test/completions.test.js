// Issue #13: semantic completions relayed to TS over the twins.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ZtsProject } from "@zestty/language-server";

const SRC = `enum Shape {
  Circle { radius: number },
  Square { side: number },
}

const picked = Shape.Circle(2);
export const twice = picked;
`;

function projectWith(text) {
  const dir = mkdtempSync(join(tmpdir(), "zts-compl-"));
  const project = new ZtsProject(dir);
  const path = join(dir, "c.zts");
  project.upsert(path, text);
  return {
    project,
    path,
    cleanup: () => rmSync(dir, { recursive: true, force: true }),
  };
}

test("member completions after a full dot-prefix (compiling state)", () => {
  const { project, path, cleanup } = projectWith(SRC);
  try {
    // `Shape.Ci|` — replace the line, still compiles.
    const lines = SRC.split("\n");
    const line = 5; // `const picked = Shape.Circle(2);`
    const character = lines[line].indexOf("Circle") + 2;
    const items = project.completions(path, { line, character });
    const labels = items.map((i) => i.label);
    assert.ok(labels.includes("Circle"), `missing Circle in ${labels}`);
    assert.ok(labels.includes("Square"), `missing Square in ${labels}`);
  } finally {
    cleanup();
  }
});

test("member completions at a bare dot (placeholder recovery path)", () => {
  const broken = SRC.replace("Shape.Circle(2)", "Shape.");
  const { project, path, cleanup } = projectWith(broken);
  try {
    const lines = broken.split("\n");
    const line = 5;
    const character = lines[line].indexOf("Shape.") + "Shape.".length;
    const items = project.completions(path, { line, character });
    const labels = items.map((i) => i.label);
    assert.ok(labels.includes("Circle"), `missing Circle in ${labels}`);
    assert.ok(!labels.includes("__ztsC"), "placeholder leaked");
    // The real document must be restored afterwards.
    assert.equal(project.docs.get(path).text, broken);
  } finally {
    cleanup();
  }
});

test("scope completions include locals and zts keywords", () => {
  const src = SRC + "const mat = picked;\nexport const x = pic;\n";
  const { project, path, cleanup } = projectWith(src);
  try {
    const lines = src.split("\n");
    const line = lines.length - 2; // `export const x = pic;`
    const character = lines[line].indexOf("pic") + 3;
    const items = project.completions(path, { line, character });
    const labels = items.map((i) => i.label);
    assert.ok(labels.includes("picked"), `missing picked in ${labels}`);

    // keyword items for a `mat` prefix (not after a dot)
    const line2 = lines.length - 3; // `const mat = picked;` — complete after `mat`
    const char2 = lines[line2].indexOf("mat") + 3;
    const kw = project.completions(path, { line: line2, character: char2 });
    assert.ok(
      kw.some((i) => i.label === "match" && i.kind === 14),
      "missing match keyword item",
    );
  } finally {
    cleanup();
  }
});
