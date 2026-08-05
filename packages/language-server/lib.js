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

    const configPath = ts.findConfigFile(rootDir, ts.sys.fileExists);
    let options = {
      strict: true,
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.ESNext,
      moduleResolution: ts.ModuleResolutionKind.Bundler,
      noEmit: true,
    };
    if (configPath) {
      const parsed = ts.parseJsonConfigFileContent(
        ts.readConfigFile(configPath, ts.sys.readFile).config,
        ts.sys,
        dirname(configPath),
      );
      options = { ...parsed.options, noEmit: true };
    }
    this.options = options;

    const project = this;
    this.service = ts.createLanguageService({
      getScriptFileNames: () => [...project.docs.keys()].map(twinName),
      getScriptVersion: (file) => {
        const doc = project.docs.get(originalName(file));
        if (doc) return String(doc.twinVersion);
        return String(statVersion(file));
      },
      getScriptSnapshot: (file) => {
        const doc = project.docs.get(originalName(file));
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
        project.docs.has(originalName(file)) || ts.sys.fileExists(file),
      readFile: (file) => {
        const doc = project.docs.get(originalName(file));
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
      twinVersion: (prev?.twinVersion ?? 0) + 1,
      consumer: null,
      error: null,
    };
    try {
      const out = compile(text, path, { tsx: path.endsWith(".ztsx") });
      doc.twin = out.code;
      doc.consumer = new SourceMapConsumer(JSON.parse(out.map));
    } catch (err) {
      doc.error = String(err.message);
      // Keep the previous twin (if any) so hover etc. degrade gracefully.
    }
    this.docs.set(path, doc);
    return doc;
  }

  close(path) {
    this.docs.delete(path);
  }

  /** original .zts position → offset in the twin, or null. */
  toTwinOffset(path, position) {
    const doc = this.docs.get(path);
    if (!doc?.consumer || doc.twin == null) return null;
    const gen = doc.consumer.generatedPositionFor({
      source: path,
      line: position.line + 1,
      column: position.character,
      bias: SourceMapConsumer.LEAST_UPPER_BOUND,
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
      const original = originalName(def.fileName);
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

/** foo.zts → foo.zts.ts (virtual twin name; never collides with real files). */
export function twinName(path) {
  return /\.ztsx?$/.test(path) ? `${path}.ts` : path;
}

function originalName(file) {
  return file.replace(/\.zts(x?)\.ts$/, ".zts$1");
}

const statVersions = new Map();
function statVersion(file) {
  // Good enough for on-disk files that rarely change mid-session.
  try {
    const mtime = ts.sys.getModifiedTime?.(file)?.getTime() ?? 0;
    statVersions.set(file, mtime);
    return mtime;
  } catch {
    return 0;
  }
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
