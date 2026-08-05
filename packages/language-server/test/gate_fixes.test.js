// Regression tests for the Phase 3 review-gate findings.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ZtsProject, twinName } from "@zestty/language-server";

function tempProject() {
  const dir = mkdtempSync(join(tmpdir(), "zts-lspgate-"));
  return { dir, cleanup: () => rmSync(dir, { recursive: true, force: true }) };
}

const DEP = `export enum Kind {
  A { n: number },
  B { s: string },
}
`;

const USE = `import { Kind } from "./dep";

export const v = match (Kind.A(1)) {
  A { n } => n,
  B { s } => s.length,
};
`;

test("C2: twin naming matches zts-check (in-place .ts / .tsx)", () => {
  assert.equal(twinName("/p/foo.zts"), "/p/foo.ts");
  assert.equal(twinName("/p/foo.ztsx"), "/p/foo.tsx");
});

test("C2: extensionless cross-file imports resolve between open docs", () => {
  const { dir, cleanup } = tempProject();
  try {
    const project = new ZtsProject(dir);
    project.upsert(join(dir, "dep.zts"), DEP);
    project.upsert(join(dir, "use.zts"), USE);
    assert.deepEqual(project.diagnostics(join(dir, "use.zts")), []);
  } finally {
    cleanup();
  }
});

test("C2: live document shadows a stale committed twin on disk", () => {
  const { dir, cleanup } = tempProject();
  try {
    // Stale committed twin says string; the OPEN .zts says number.
    writeFileSync(join(dir, "lib.ts"), 'export const val: string = "STALE";\n');
    writeFileSync(join(dir, "lib.zts"), "export const val: number = 42;\n");
    const project = new ZtsProject(dir);
    project.upsert(join(dir, "lib.zts"), "export const val: number = 42;\n");
    project.upsert(
      join(dir, "use.zts"),
      'import { val } from "./lib";\nexport const twice: number = val * 2;\n',
    );
    // Against the STALE twin this errors (string * 2); against the live
    // doc it's clean. The live doc must win.
    assert.deepEqual(project.diagnostics(join(dir, "use.zts")), []);
  } finally {
    cleanup();
  }
});

test("H5: ambient globals from the tsconfig program are visible", () => {
  const { dir, cleanup } = tempProject();
  try {
    writeFileSync(
      join(dir, "tsconfig.json"),
      JSON.stringify({
        compilerOptions: { strict: true, noEmit: true },
        include: ["**/*.d.ts"],
      }),
    );
    writeFileSync(
      join(dir, "globals.d.ts"),
      "declare const APP_VERSION: string;\n",
    );
    const project = new ZtsProject(dir);
    const path = join(dir, "g.zts");
    project.upsert(path, "export const v: string = APP_VERSION;\n");
    assert.deepEqual(project.diagnostics(path), []);
  } finally {
    cleanup();
  }
});

test("M7: hover never answers with a neighboring symbol (GLB bias)", () => {
  const { dir, cleanup } = tempProject();
  try {
    const project = new ZtsProject(dir);
    const path = join(dir, "h.zts");
    const src = `enum Shape {
  Circle { radius: number },
}

const c = Shape.Circle(2);
export const area = match (c) {
  Circle { radius } => radius * 2,
};
`;
    project.upsert(path, src);
    // Inside `Shape` on line 4 — LUB used to answer with `.Circle`'s type.
    const line = 4;
    const character = src.split("\n")[line].indexOf("Shape") + 2;
    const hover = project.hover(path, { line, character });
    if (hover) {
      assert.doesNotMatch(
        hover.contents.value,
        /\(property\) Circle/,
        "hover answered with the NEIGHBORING symbol",
      );
    }
    // Negative positions must not throw.
    assert.equal(project.hover(path, { line: -1, character: 0 }), null);
  } finally {
    cleanup();
  }
});

test("M11: compile failure keeps hover coherent on last-good state", () => {
  const { dir, cleanup } = tempProject();
  try {
    const project = new ZtsProject(dir);
    const path = join(dir, "m.zts");
    const good = "export const n: number = 42;\n";
    project.upsert(path, good, 1);
    const hoverBefore = project.hover(path, {
      line: 0,
      character: good.indexOf("n:"),
    });
    assert.ok(hoverBefore, "baseline hover");

    // Broken edit: compile fails, diagnostics show the compile error, and
    // hover still answers from the coherent last-good twin+map pair.
    project.upsert(
      path,
      "export const n: number = match (t) { K { v: bad } => v };\n",
      2,
    );
    const diags = project.diagnostics(path);
    assert.equal(diags.length, 1);
    assert.match(diags[0].message, /shorthand identifiers/);
    const hoverAfter = project.hover(path, {
      line: 0,
      character: good.indexOf("n:"),
    });
    assert.ok(hoverAfter, "hover must degrade to last-good, not die");
  } finally {
    cleanup();
  }
});
