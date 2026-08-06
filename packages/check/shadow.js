// Shadow-tree svelte-check: template bindings against zts script members.
//
// svelte-check type-checks the ORIGINAL source and keys the language off
// the original `lang` attribute, so `lang="zts"` blocks are invisible to
// it. The fix: mirror the project into `.zts-check/shadow/` (symlinks for
// everything untouched, node_modules included), rewrite each zts component
// with its COMPILED script and `lang="ts"`, compile `.zts` modules to
// twins, run svelte-check over the shadow, and remap its diagnostics:
//   - template lines above the script: identity;
//   - lines inside the compiled script: through the stage-1 sourcemap;
//   - template lines below the script: shifted by the compiled/original
//     line-count delta.

import { compile } from "@zestty/native";
import { SourceMapConsumer } from "source-map-js";
import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, relative, resolve } from "node:path";

const SKIP_DIRS = new Set([".git", "target", ".zts-check"]);
// Symlinked wholesale rather than walked: generated output that the shadow
// must still SEE — SvelteKit's tsconfig `extends ./.svelte-kit/tsconfig.json`
// (issue #15), and module resolution needs node_modules.
const LINK_DIRS = new Set(["node_modules", ".svelte-kit"]);
const SVELTE_ZTS_SCRIPT =
  /<script([^>]*)\blang\s*=\s*["']zts["']([^>]*)>([\s\S]*?)<\/script>/g;

const countNewlines = (s) => (s.match(/\n/g) ?? []).length;

/**
 * Rewrite one component: every `lang="zts"` script gets its compiled body
 * and `lang="ts"`. Returns null when the component has no zts scripts.
 */
export function transformComponent(source, filePath) {
  const blocks = [];
  const errors = [];
  let out = "";
  let last = 0;
  let cumDelta = 0;

  for (const m of source.matchAll(SVELTE_ZTS_SCRIPT)) {
    const [full, before, after, content] = m;
    let compiled;
    try {
      compiled = compile(content, filePath, { tsx: false });
    } catch (err) {
      errors.push(String(err.message).trimEnd());
      continue;
    }
    const openTag = `<script${before}lang="ts"${after}>`;
    out += source.slice(last, m.index);
    out += openTag;
    // Compiled code goes on its OWN lines: line 1 of the compiled text
    // sits exactly at shadow line `origContentStart` (+ cumulative delta).
    out += "\n";
    // First content line in the ORIGINAL file (1-based): everything up to
    // and including the open tag, plus one.
    const origContentStart =
      countNewlines(source.slice(0, m.index)) +
      countNewlines(full.slice(0, full.indexOf(">") + 1)) +
      2;
    const origLines = countNewlines(content);
    const compLines = countNewlines(compiled.code) + 1; // + the "\n" above
    blocks.push({
      origContentStart,
      origLines,
      compLines,
      cumDeltaBefore: cumDelta,
      consumer: new SourceMapConsumer(JSON.parse(compiled.map)),
    });
    cumDelta += compLines - origLines;
    out += compiled.code;
    out += "</script>";
    last = m.index + full.length;
  }

  if (blocks.length === 0 && errors.length === 0) return null;
  out += source.slice(last);
  return { code: out, blocks, errors };
}

/** Map a 1-based (line, col) in the SHADOW component back to the original. */
export function remapComponentPosition(blocks, line, column) {
  let cum = 0;
  for (const b of blocks) {
    const shadowStart = b.origContentStart + cum;
    const shadowEnd = shadowStart + b.compLines - 1;
    if (line < shadowStart) break;
    if (line <= shadowEnd) {
      const orig = b.consumer.originalPositionFor({
        line: line - shadowStart + 1,
        column: Math.max(0, column - 1),
      });
      if (orig.line != null) {
        // Original content line 1 is the empty tail of the tag line, so
        // content line k lives at file line (origContentStart - 2) + k.
        return {
          line: b.origContentStart - 2 + orig.line,
          column: orig.column + 1,
        };
      }
      // Unmapped generated scaffolding: point at the script start.
      return { line: b.origContentStart, column: 1 };
    }
    cum += b.compLines - b.origLines;
  }
  return { line: line - cum, column };
}

/**
 * Mirror the project into shadowDir. Returns per-component remap info and
 * compile errors.
 */
export function buildShadow(root, shadowDir) {
  rmSync(shadowDir, { recursive: true, force: true });
  mkdirSync(shadowDir, { recursive: true });
  /** shadow rel path -> { originalPath, blocks } */
  const components = new Map();
  const errors = [];

  const walk = (dir, shadow) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const src = join(dir, entry.name);
      const dst = join(shadow, entry.name);
      if (entry.isSymbolicLink()) {
        continue; // avoid cycles; symlinked trees are out of scope (documented)
      }
      if (entry.isDirectory()) {
        if (SKIP_DIRS.has(entry.name)) continue;
        if (LINK_DIRS.has(entry.name)) {
          symlinkSync(src, dst, "dir");
          continue;
        }
        mkdirSync(dst, { recursive: true });
        walk(src, dst);
      } else if (entry.name.endsWith(".svelte")) {
        const source = readFileSync(src, "utf8");
        const transformed = transformComponent(source, src);
        if (transformed == null) {
          symlinkSync(src, dst);
        } else {
          errors.push(
            ...transformed.errors.map((e) => ({ file: src, message: e })),
          );
          writeFileSync(dst, transformed.code);
          components.set(relative(shadowDir, dst), {
            originalPath: src,
            blocks: transformed.blocks,
          });
        }
      } else if (/\.ztsx?$/.test(entry.name)) {
        try {
          const compiled = compile(readFileSync(src, "utf8"), src, {
            tsx: entry.name.endsWith(".ztsx"),
          });
          writeFileSync(dst.replace(/\.zts(x?)$/, ".ts$1"), compiled.code);
        } catch (err) {
          errors.push({ file: src, message: String(err.message).trimEnd() });
        }
      } else {
        symlinkSync(src, dst);
      }
    }
  };
  walk(root, shadowDir);
  return { components, errors };
}

function findSvelteCheck(root) {
  for (const base of [join(root, "package.json"), import.meta.url]) {
    try {
      const require = createRequire(base);
      const pkg = require.resolve("svelte-check/package.json");
      const manifest = JSON.parse(readFileSync(pkg, "utf8"));
      const bin =
        typeof manifest.bin === "string"
          ? manifest.bin
          : manifest.bin["svelte-check"];
      return join(dirname(pkg), bin);
    } catch {
      // try the next base
    }
  }
  return null;
}

// `--output machine`: TIMESTAMP SEVERITY "relpath" LINE:COL "message"
const MACHINE_DIAG = /^\d+\s+(ERROR|WARNING)\s+"(.+?)"\s+(\d+):(\d+)\s+"(.*)"$/;

/**
 * Run svelte-check over the shadow tree and return diagnostics remapped to
 * the ORIGINAL component paths/positions. Returns null when svelte-check
 * is not installed anywhere we can find it.
 */
export function runSvelteCheck(root, shadowDir, components) {
  const bin = findSvelteCheck(root);
  if (bin == null) return null;

  let stdout = "";
  try {
    stdout = execFileSync(
      process.execPath,
      [
        bin,
        "--workspace",
        shadowDir,
        "--output",
        "machine",
        "--threshold",
        "error",
      ],
      { encoding: "utf8", cwd: shadowDir },
    );
  } catch (err) {
    stdout = `${err.stdout ?? ""}${err.stderr ?? ""}`;
  }

  const diagnostics = [];
  for (const line of stdout.split("\n")) {
    const m = MACHINE_DIAG.exec(line.trim());
    if (!m) continue;
    const [, severity, relPath, lineNo, colNo, message] = m;
    if (severity !== "ERROR") continue;
    const component = components.get(relPath);
    const msg = message.replace(/\\"/g, '"');
    if (!component) {
      // A diagnostic in a symlinked (untouched) file: report as-is against
      // the original tree.
      diagnostics.push({
        file: resolve(root, relPath),
        line: Number(lineNo),
        column: Number(colNo),
        message: `error (svelte-check): ${msg}`,
      });
      continue;
    }
    const pos = remapComponentPosition(
      component.blocks,
      Number(lineNo),
      Number(colNo),
    );
    diagnostics.push({
      file: component.originalPath,
      line: pos.line,
      column: pos.column,
      message: `error (svelte-check): ${msg}`,
    });
  }
  return diagnostics;
}
