import { compile } from "@zestty/native";
import { SourceMapConsumer } from "source-map-js";
import { execFileSync } from "node:child_process";
import {
  readdirSync,
  readFileSync,
  writeFileSync,
  unlinkSync,
  existsSync,
} from "node:fs";
import { createRequire } from "node:module";
import { join, relative, resolve, dirname, basename } from "node:path";

const SKIP_DIRS = new Set([
  "node_modules",
  ".git",
  "target",
  "dist",
  "build",
  ".svelte-kit",
  ".zts-check",
]);

/** Recursively collect .zts/.ztsx/.svelte files under root. */
export function scan(root) {
  const out = { zts: [], svelte: [] };
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory()) {
        if (!SKIP_DIRS.has(entry.name)) walk(join(dir, entry.name));
      } else if (/\.ztsx?$/.test(entry.name)) {
        out.zts.push(join(dir, entry.name));
      } else if (entry.name.endsWith(".svelte")) {
        out.svelte.push(join(dir, entry.name));
      }
    }
  };
  walk(root);
  return out;
}

const SVELTE_ZTS_SCRIPT =
  /<script([^>]*\blang\s*=\s*["']zts["'][^>]*)>([\s\S]*?)<\/script>/;

/**
 * Strip a trailing sourceMappingURL comment: committed twins carry one,
 * fresh compiles don't, and staleness comparison must not care.
 */
function stripMapComment(code) {
  return code.replace(/\n?\/\/# sourceMappingURL=\S*\s*$/, "").trimEnd();
}

/**
 * Plan and materialize the shadow twins for one project.
 *
 * Twins are written IN PLACE (`foo.zts` → `foo.ts`) so that extensionless
 * relative imports between modules resolve exactly as they will for
 * consumers in committed-twins mode. If a committed twin already exists:
 * - identical content (modulo sourceMappingURL) → reuse, don't touch;
 * - different content → that's a STALE TWIN, reported as an error.
 */
export function materialize(root) {
  const { zts, svelte } = scan(root);
  const twins = []; // { twinPath, mapJson, originalPath, created, scriptOffset }
  const errors = [];

  for (const file of zts) {
    const source = readFileSync(file, "utf8");
    let compiled;
    try {
      compiled = compile(source, file, { tsx: file.endsWith(".ztsx") });
    } catch (err) {
      errors.push({ file, message: String(err.message).trimEnd() });
      continue;
    }
    const twinPath = file.replace(/\.zts(x?)$/, ".ts$1");
    if (existsSync(twinPath)) {
      const existing = stripMapComment(readFileSync(twinPath, "utf8"));
      if (existing === stripMapComment(compiled.code)) {
        twins.push({
          twinPath,
          mapJson: compiled.map,
          originalPath: file,
          created: false,
          scriptOffset: 0,
        });
      } else {
        errors.push({
          file: twinPath,
          message: `stale committed twin: ${relative(root, twinPath)} does not match a fresh compile of ${relative(root, file)} — regenerate it`,
        });
      }
      continue;
    }
    writeFileSync(twinPath, compiled.code);
    twins.push({
      twinPath,
      mapJson: compiled.map,
      originalPath: file,
      created: true,
      scriptOffset: 0,
    });
  }

  for (const file of svelte) {
    const source = readFileSync(file, "utf8");
    const m = SVELTE_ZTS_SCRIPT.exec(source);
    if (!m) continue;
    const scriptContent = m[2];
    // Line offset of the script *content* inside the component file.
    const scriptOffset = source
      .slice(0, m.index + m[0].indexOf(">") + 1)
      .split("\n").length;
    let compiled;
    try {
      compiled = compile(scriptContent, file, { tsx: false });
    } catch (err) {
      errors.push({ file, message: String(err.message).trimEnd() });
      continue;
    }
    const twinPath = join(
      dirname(file),
      `${basename(file, ".svelte")}.svelte-script.ts`,
    );
    if (existsSync(twinPath)) {
      errors.push({
        file: twinPath,
        message: `refusing to overwrite existing file for the svelte script twin of ${relative(root, file)}`,
      });
      continue;
    }
    writeFileSync(twinPath, compiled.code);
    twins.push({
      twinPath,
      mapJson: compiled.map,
      originalPath: file,
      created: true,
      scriptOffset: scriptOffset - 1,
    });
  }

  return { twins, errors };
}

function tscFromResolved(entry) {
  // typescript's exports map hides bin/tsc from require.resolve (TS 7+),
  // so resolve the package entry point and walk up to the package root.
  let dir = dirname(entry);
  while (basename(dir) !== "typescript") {
    const parent = dirname(dir);
    if (parent === dir) throw new Error("typescript package root not found");
    dir = parent;
  }
  return join(dir, "bin", "tsc");
}

function findTsc(root) {
  try {
    const require = createRequire(join(root, "package.json"));
    return tscFromResolved(require.resolve("typescript"));
  } catch {
    // Fall back to this package's own tree (repo-local dev).
    const require2 = createRequire(import.meta.url);
    return tscFromResolved(require2.resolve("typescript"));
  }
}

const TSC_DIAG = /^(.+?)\((\d+),(\d+)\): (error TS\d+: [\s\S]*)$/;

/**
 * Run tsc over the project (or the twin files when there's no tsconfig)
 * and remap diagnostics that land inside twins back to their zts origins.
 */
export function runCheck(root, twins) {
  const tsc = findTsc(root);
  const args = existsSync(join(root, "tsconfig.json"))
    ? ["-p", root, "--noEmit", "--pretty", "false"]
    : [
        "--noEmit",
        "--strict",
        "--target",
        "es2022",
        "--module",
        "esnext",
        "--moduleResolution",
        "bundler",
        "--pretty",
        "false",
        ...twins.map((t) => t.twinPath),
      ];

  let stdout = "";
  let failed = false;
  try {
    stdout = execFileSync(process.execPath, [tsc, ...args], {
      encoding: "utf8",
      cwd: root,
    });
  } catch (err) {
    failed = true;
    stdout = `${err.stdout ?? ""}${err.stderr ?? ""}`;
  }

  const byTwin = new Map(twins.map((t) => [resolve(root, t.twinPath), t]));

  const diagnostics = [];
  for (const line of stdout.split("\n")) {
    const m = TSC_DIAG.exec(line.trimEnd());
    if (!m) {
      if (line.trim()) diagnostics.push({ raw: line.trimEnd() });
      continue;
    }
    const [, file, lineNo, colNo, message] = m;
    const abs = resolve(root, file);
    const twin = byTwin.get(abs);
    if (!twin) {
      diagnostics.push({ raw: line.trimEnd() });
      continue;
    }
    const consumer = new SourceMapConsumer(JSON.parse(twin.mapJson));
    const orig = consumer.originalPositionFor({
      line: Number(lineNo),
      column: Number(colNo) - 1,
    });
    if (orig.line == null) {
      diagnostics.push({ raw: line.trimEnd() });
      continue;
    }
    diagnostics.push({
      file: twin.originalPath,
      line: orig.line + twin.scriptOffset,
      column: orig.column + 1,
      message,
    });
  }

  return { failed, diagnostics };
}

export function cleanup(twins) {
  for (const t of twins) {
    if (t.created) {
      try {
        unlinkSync(t.twinPath);
      } catch {
        // already gone — fine
      }
    }
  }
}

/** Full run. Returns an exit code. */
export function ztsCheck(root, { keep = false, log = console.error } = {}) {
  root = resolve(root);
  const { twins, errors } = materialize(root);

  let errorCount = 0;
  try {
    for (const e of errors) {
      errorCount += 1;
      log(`${relative(root, e.file)}: ${e.message}`);
    }

    const { failed, diagnostics } = runCheck(root, twins);
    for (const d of diagnostics) {
      if (d.raw != null) {
        errorCount += /error TS\d+/.test(d.raw) ? 1 : 0;
        log(d.raw);
      } else {
        errorCount += 1;
        log(`${relative(root, d.file)}(${d.line},${d.column}): ${d.message}`);
      }
    }
    if (failed && errorCount === 0) {
      // tsc failed without parseable diagnostics — surface that loudly.
      errorCount += 1;
      log("zts-check: tsc exited non-zero without diagnostics");
    }
  } finally {
    if (!keep) cleanup(twins);
  }

  if (errorCount === 0) {
    log("zts-check: clean");
  }
  return errorCount === 0 ? 0 : 1;
}
