import { compile } from "@zestty/native";
import { SourceMapConsumer } from "source-map-js";
import { buildShadow, runSvelteCheck } from "./shadow.js";
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
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
  /<script([^>]*\blang\s*=\s*["']zts["'][^>]*)>([\s\S]*?)<\/script>/g;

/**
 * Strip a trailing sourceMappingURL comment: committed twins carry one,
 * fresh compiles don't, and staleness comparison must not care.
 */
function stripMapComment(code) {
  return code
    .replace(/\r\n/g, "\n") // CRLF checkouts must not look stale
    .replace(/\n?\/\/# sourceMappingURL=\S*\s*$/, "")
    .trimEnd();
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
export function materialize(root, { svelteScriptTwins = true } = {}) {
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

  for (const file of svelteScriptTwins ? svelte : []) {
    const source = readFileSync(file, "utf8");
    // ALL lang="zts" blocks (instance + context="module"), not just the first.
    let index = 0;
    for (const m of source.matchAll(SVELTE_ZTS_SCRIPT)) {
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
        index += 1;
        continue;
      }
      const suffix = index === 0 ? "" : String(index);
      const twinPath = join(
        dirname(file),
        `${basename(file, ".svelte")}.svelte-script${suffix}.ts`,
      );
      index += 1;
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
  // --listFiles: every twin MUST appear in tsc's program, or a .zts outside
  // the tsconfig's include silently escapes the gate (false-green CI).
  const args = existsSync(join(root, "tsconfig.json"))
    ? ["-p", root, "--noEmit", "--pretty", "false", "--listFiles"]
    : [
        "--noEmit",
        "--strict",
        "--target",
        "es2022",
        "--module",
        "esnext",
        "--moduleResolution",
        "bundler",
        "--jsx",
        "preserve",
        "--pretty",
        "false",
        "--listFiles",
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
  const consumers = new Map(); // twinPath -> SourceMapConsumer (built once)
  const consumerFor = (twin) => {
    let c = consumers.get(twin.twinPath);
    if (!c) {
      c = new SourceMapConsumer(JSON.parse(twin.mapJson));
      consumers.set(twin.twinPath, c);
    }
    return c;
  };

  const seenInProgram = new Set();
  const diagnostics = [];
  for (const line of stdout.split("\n")) {
    const trimmed = line.trimEnd();
    // --listFiles lines are bare absolute paths; consume them silently.
    const asPath = resolve(root, trimmed);
    if (byTwin.has(asPath) && !TSC_DIAG.test(trimmed)) {
      seenInProgram.add(asPath);
      continue;
    }
    if (/^\//.test(trimmed) && !TSC_DIAG.test(trimmed) && existsSync(trimmed)) {
      continue; // some other program file listing
    }
    const m = TSC_DIAG.exec(trimmed);
    if (!m) {
      if (trimmed.trim()) diagnostics.push({ raw: trimmed });
      continue;
    }
    const [, file, lineNo, colNo, message] = m;
    const abs = resolve(root, file);
    const twin = byTwin.get(abs);
    if (!twin) {
      diagnostics.push({ raw: trimmed });
      continue;
    }
    const orig = consumerFor(twin).originalPositionFor({
      line: Number(lineNo),
      column: Number(colNo) - 1,
    });
    if (orig.line == null) {
      diagnostics.push({ raw: trimmed });
      continue;
    }
    diagnostics.push({
      file: twin.originalPath,
      line: orig.line + twin.scriptOffset,
      column: orig.column + 1,
      message,
    });
  }

  // C1: a twin tsc never loaded means its .zts escaped the gate entirely.
  for (const t of twins) {
    if (!seenInProgram.has(resolve(root, t.twinPath))) {
      diagnostics.push({
        file: t.originalPath,
        line: 1,
        column: 1,
        message: `error TS0: not covered by the project tsconfig (its twin ${relative(root, t.twinPath)} is outside include/files) — widen the tsconfig so this file is actually checked`,
      });
    }
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

const MANIFEST_DIR = ".zts-check";
const MANIFEST = "created.json";

/**
 * A manifest left behind means a previous run died before cleanup (SIGKILL,
 * OOM). Its entries are twins WE created — delete them instead of adopting
 * byte-identical leftovers as committed twins (which would later surface as
 * bogus "stale committed twin" errors the user never created).
 */
function recoverLeaks(root, log) {
  const manifestPath = join(root, MANIFEST_DIR, MANIFEST);
  if (!existsSync(manifestPath)) return;
  try {
    const created = JSON.parse(readFileSync(manifestPath, "utf8"));
    for (const twin of created) {
      if (existsSync(twin)) {
        unlinkSync(twin);
        log(
          `zts-check: removed twin leaked by an interrupted run: ${relative(root, twin)}`,
        );
      }
    }
  } finally {
    rmSync(join(root, MANIFEST_DIR), { recursive: true, force: true });
  }
}

function writeManifest(root, twins) {
  const created = twins.filter((t) => t.created).map((t) => t.twinPath);
  if (created.length === 0) return;
  mkdirSync(join(root, MANIFEST_DIR), { recursive: true });
  writeFileSync(join(root, MANIFEST_DIR, MANIFEST), JSON.stringify(created));
}

function removeManifest(root) {
  rmSync(join(root, MANIFEST_DIR), { recursive: true, force: true });
}

const HAS_ZTS_SCRIPT = /<script[^>]*\blang\s*=\s*["']zts["']/;

/** Full run. Returns an exit code. */
export function ztsCheck(
  root,
  { keep = false, svelte = true, log = console.error } = {},
) {
  root = resolve(root);
  if (!existsSync(root)) {
    log(`zts-check: no such directory: ${root}`);
    return 2;
  }

  recoverLeaks(root, log);

  // Template checking: when svelte-check is available, components are
  // checked WHOLE (template bindings included) over a shadow tree, and the
  // script-twin path is skipped for them to avoid double-reporting.
  const ztsComponents = svelte
    ? scan(root).svelte.filter((f) =>
        HAS_ZTS_SCRIPT.test(readFileSync(f, "utf8")),
      )
    : [];
  const useShadow = ztsComponents.length > 0;

  let twins = [];
  let errorCount = 0;

  // Ctrl-C / CI cancellation must not leave twins in the tree.
  const onSignal = (signal) => {
    cleanup(twins);
    removeManifest(root);
    process.exit(signal === "SIGINT" ? 130 : 143);
  };
  const SIGNALS = ["SIGINT", "SIGTERM", "SIGHUP"];
  for (const sig of SIGNALS) process.on(sig, onSignal);

  try {
    const materialized = materialize(root, { svelteScriptTwins: !useShadow });
    twins = materialized.twins;
    writeManifest(root, twins);

    for (const e of materialized.errors) {
      errorCount += 1;
      log(`${relative(root, e.file)}: ${e.message}`);
    }

    if (twins.length === 0 && !useShadow) {
      if (errorCount === 0) log("zts-check: nothing to check");
      return errorCount === 0 ? 0 : 1;
    }

    if (twins.length > 0) {
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
    }

    if (useShadow) {
      const shadowDir = join(root, MANIFEST_DIR, "shadow");
      // Shadow compile errors duplicate the module-twin errors already
      // reported above; components-only errors surface via svelte-check.
      const { components } = buildShadow(root, shadowDir);
      const svelteDiags = runSvelteCheck(root, shadowDir, components);
      if (svelteDiags == null) {
        // Script twins were skipped in anticipation — fall back so the
        // scripts are still checked, and say what was NOT checked.
        log(
          'zts-check: svelte-check not found — template bindings in lang="zts" components were NOT checked (scripts still are; install svelte-check for full coverage)',
        );
        const fallback = materialize(root, { svelteScriptTwins: true });
        const scriptTwins = fallback.twins.filter((t) =>
          t.originalPath.endsWith(".svelte"),
        );
        twins.push(...scriptTwins);
        const { diagnostics } = runCheck(root, scriptTwins);
        for (const d of diagnostics) {
          if (d.raw != null) {
            errorCount += /error TS\d+/.test(d.raw) ? 1 : 0;
            log(d.raw);
          } else {
            errorCount += 1;
            log(
              `${relative(root, d.file)}(${d.line},${d.column}): ${d.message}`,
            );
          }
        }
      } else {
        for (const d of svelteDiags) {
          errorCount += 1;
          log(`${relative(root, d.file)}(${d.line},${d.column}): ${d.message}`);
        }
      }
      if (!keep) rmSync(shadowDir, { recursive: true, force: true });
    }
  } finally {
    for (const sig of SIGNALS) process.off(sig, onSignal);
    if (!keep) {
      cleanup(twins);
      removeManifest(root);
    }
  }

  if (errorCount === 0) {
    log("zts-check: clean");
  }
  return errorCount === 0 ? 0 : 1;
}
