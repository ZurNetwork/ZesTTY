import { compile } from "@zestty/native";
import { SourceMapConsumer } from "source-map-js";
import { createRequire } from "node:module";
import { dirname } from "node:path";

const require = createRequire(import.meta.url);
/** @type {import("typescript")} */
const ts = require("typescript");

/**
 * The testable core of the ZesTTY language server: tracks open `.zts`
 * documents, compiles each into a virtual `.ts` twin, serves the twins to
 * a TypeScript LanguageService (shadowing any committed on-disk twins),
 * and converts positions in both directions through the sourcemaps.
 *
 * Positions are LSP-style: zero-based line and character.
 */
export class ZtsProject {
  constructor(rootDir) {
    this.rootDir = rootDir;
    /** @type {Map<string, {text: string, version: number, twin: string|null, twinVersion: number, consumer: any, error: string|null}>} */
    this.docs = new Map();
    /** twin path -> original .zts path (reverse index; twin names are
     * in-place `foo.ts`, IDENTICAL to zts-check's, so the two tools agree
     * and extensionless imports resolve — virtual twins shadow any
     * committed/stale on-disk file of the same name). */
    this.twinToDoc = new Map();

    const configPath = ts.findConfigFile(rootDir, ts.sys.fileExists);
    let options = {
      strict: true,
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.ESNext,
      moduleResolution: ts.ModuleResolutionKind.Bundler,
      noEmit: true,
    };
    /** Files from the project tsconfig (ambient .d.ts, app.d.ts, …) —
     * without them, `declare global` and env types are invisible and the
     * LSP reports false "Cannot find name" errors. */
    this.projectFiles = [];
    if (configPath) {
      const parsed = ts.parseJsonConfigFileContent(
        ts.readConfigFile(configPath, ts.sys.readFile).config,
        ts.sys,
        dirname(configPath),
      );
      options = { ...parsed.options, noEmit: true };
      this.projectFiles = parsed.fileNames;
    }
    this.options = options;

    const project = this;
    const docFor = (file) => {
      const original = project.twinToDoc.get(file);
      return original != null ? project.docs.get(original) : undefined;
    };
    this.service = ts.createLanguageService({
      getScriptFileNames: () => [
        ...new Set([
          ...project.projectFiles,
          ...[...project.docs.keys()].map(twinName),
        ]),
      ],
      getScriptVersion: (file) => {
        const doc = docFor(file);
        if (doc) return String(doc.twinVersion);
        return String(ts.sys.getModifiedTime?.(file)?.getTime() ?? 0);
      },
      getScriptSnapshot: (file) => {
        const doc = docFor(file);
        if (doc) {
          return doc.twin != null
            ? ts.ScriptSnapshot.fromString(doc.twin)
            : undefined;
        }
        const content = ts.sys.readFile(file);
        return content == null
          ? undefined
          : ts.ScriptSnapshot.fromString(content);
      },
      getCurrentDirectory: () => project.rootDir,
      getCompilationSettings: () => project.options,
      getDefaultLibFileName: (o) => ts.getDefaultLibFilePath(o),
      fileExists: (file) =>
        project.twinToDoc.has(file) || ts.sys.fileExists(file),
      readFile: (file) => {
        const doc = docFor(file);
        return doc?.twin ?? ts.sys.readFile(file);
      },
      readDirectory: ts.sys.readDirectory,
      directoryExists: ts.sys.directoryExists,
      getDirectories: ts.sys.getDirectories,
    });
  }

  /** Open or update a document. Returns compile diagnostics (may be []). */
  upsert(path, text, version = 1) {
    const prev = this.docs.get(path);
    const doc = {
      text,
      version,
      twin: prev?.twin ?? null,
      twinVersion: prev?.twinVersion ?? 1,
      consumer: prev?.consumer ?? null,
      error: null,
    };
    try {
      const out = compile(text, path, { tsx: path.endsWith(".ztsx") });
      if (out.code !== doc.twin) {
        doc.twin = out.code;
        doc.twinVersion += 1;
      }
      doc.consumer = new SourceMapConsumer(JSON.parse(out.map));
    } catch (err) {
      doc.error = String(err.message);
      // Keep the previous twin AND its matching consumer together, so
      // hover/defs degrade to last-good coherently instead of mixing a
      // stale twin with no mapper.
    }
    this.docs.set(path, doc);
    this.twinToDoc.set(twinName(path), path);
    return doc;
  }

  close(path) {
    this.twinToDoc.delete(twinName(path));
    this.docs.delete(path);
  }

  /** original .zts position → offset in the twin, or null. */
  toTwinOffset(path, position) {
    const doc = this.docs.get(path);
    if (!doc?.consumer || doc.twin == null) return null;
    if (position.line < 0 || position.character < 0) return null;
    // GREATEST_LOWER_BOUND: with statement-granular maps, snapping FORWARD
    // (LUB) answers hover with a confidently wrong neighboring symbol;
    // GLB degrades to null instead. No hover beats a lying hover.
    const gen = doc.consumer.generatedPositionFor({
      source: path,
      line: position.line + 1,
      column: position.character,
      bias: SourceMapConsumer.GREATEST_LOWER_BOUND,
    });
    if (gen.line == null) return null;
    const lines = doc.twin.split("\n");
    let offset = 0;
    for (let i = 0; i < gen.line - 1; i++) offset += lines[i].length + 1;
    return offset + gen.column;
  }

  /** twin offset/span → original .zts LSP range, or null. */
  toOriginalRange(path, start, length) {
    const doc = this.docs.get(path);
    if (!doc?.consumer || doc.twin == null) return null;
    const startLc = offsetToLineCol(doc.twin, start);
    const endLc = offsetToLineCol(doc.twin, start + (length ?? 0));
    const s = doc.consumer.originalPositionFor({
      line: startLc.line + 1,
      column: startLc.col,
    });
    if (s.line == null) return null;
    const e = doc.consumer.originalPositionFor({
      line: endLc.line + 1,
      column: endLc.col,
    });
    const startPos = { line: s.line - 1, character: s.column };
    const endPos =
      e.line != null && (e.line > s.line || e.column > s.column)
        ? { line: e.line - 1, character: e.column }
        : endOfToken(this.docs.get(path).text, startPos);
    return { start: startPos, end: endPos };
  }

  /**
   * All diagnostics for one document, as LSP-shaped objects with ranges in
   * the ORIGINAL .zts text.
   */
  diagnostics(path) {
    const doc = this.docs.get(path);
    if (!doc) return [];

    if (doc.error != null) {
      return [compileErrorToDiagnostic(doc.error, path, doc.text)];
    }

    const twin = twinName(path);
    const all = [
      ...this.service.getSyntacticDiagnostics(twin),
      ...this.service.getSemanticDiagnostics(twin),
    ];
    const out = [];
    for (const d of all) {
      if (d.start == null) continue;
      const range =
        this.toOriginalRange(path, d.start, d.length ?? 0) ??
        wholeFirstLine(doc.text);
      out.push({
        range,
        severity: d.category === ts.DiagnosticCategory.Error ? 1 : 2,
        code: d.code,
        source: "zts",
        message: ts.flattenDiagnosticMessageText(d.messageText, "\n"),
      });
    }
    return out;
  }

  /** Hover info at an original position, remapped. */
  hover(path, position) {
    const offset = this.toTwinOffset(path, position);
    if (offset == null) return null;
    const info = this.service.getQuickInfoAtPosition(twinName(path), offset);
    if (!info) return null;
    const text = ts.displayPartsToString(info.displayParts);
    const docs = ts.displayPartsToString(info.documentation ?? []);
    const range = this.toOriginalRange(
      path,
      info.textSpan.start,
      info.textSpan.length,
    );
    return {
      contents: {
        kind: "markdown",
        value: "```typescript\n" + text + "\n```" + (docs ? `\n\n${docs}` : ""),
      },
      ...(range ? { range } : {}),
    };
  }

  /** Definitions from an original position; twin hits remap, disk files pass through. */
  definitions(path, position) {
    const offset = this.toTwinOffset(path, position);
    if (offset == null) return [];
    const defs =
      this.service.getDefinitionAtPosition(twinName(path), offset) ?? [];
    const out = [];
    for (const def of defs) {
      const original = this.twinToDoc.get(def.fileName) ?? def.fileName;
      if (this.docs.has(original)) {
        const range = this.toOriginalRange(
          original,
          def.textSpan.start,
          def.textSpan.length,
        );
        if (range) out.push({ path: original, range });
      } else if (ts.sys.fileExists(def.fileName)) {
        const content = ts.sys.readFile(def.fileName) ?? "";
        const s = offsetToLineCol(content, def.textSpan.start);
        const e = offsetToLineCol(
          content,
          def.textSpan.start + def.textSpan.length,
        );
        out.push({
          path: def.fileName,
          range: {
            start: { line: s.line, character: s.col },
            end: { line: e.line, character: e.col },
          },
        });
      }
    }
    return out;
  }
}

/**
 * foo.zts → foo.ts, foo.ztsx → foo.tsx — the SAME in-place naming
 * zts-check uses, so extensionless imports resolve identically in both
 * tools and the virtual twin shadows any committed (possibly stale)
 * on-disk twin. Reverse lookups go through ZtsProject.twinToDoc.
 */
export function twinName(path) {
  return path.replace(/\.zts(x?)$/, ".ts$1");
}

function offsetToLineCol(text, offset) {
  let line = 0;
  let last = 0;
  for (let i = 0; i < offset && i < text.length; i++) {
    if (text[i] === "\n") {
      line += 1;
      last = i + 1;
    }
  }
  return { line, col: Math.min(offset, text.length) - last };
}

function endOfToken(text, startPos) {
  const lines = text.split("\n");
  const line = lines[startPos.line] ?? "";
  let end = startPos.character;
  while (end < line.length && /[\w$]/.test(line[end])) end += 1;
  return {
    line: startPos.line,
    character: Math.max(end, startPos.character + 1),
  };
}

function wholeFirstLine(text) {
  const len = text.split("\n", 1)[0]?.length ?? 1;
  return { start: { line: 0, character: 0 }, end: { line: 0, character: len } };
}

/** Parse ` --> path:line:col` spans out of a rendered zts diagnostic. */
function compileErrorToDiagnostic(message, path, text) {
  const m = /-->\s+.*:(\d+):(\d+)/.exec(message);
  let range = wholeFirstLine(text);
  if (m) {
    const line = Number(m[1]) - 1;
    const character = Number(m[2]) - 1;
    range = {
      start: { line, character },
      end: endOfToken(text, { line, character }),
    };
  }
  return { range, severity: 1, source: "zts", message: message.trim() };
}
