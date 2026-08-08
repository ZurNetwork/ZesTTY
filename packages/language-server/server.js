#!/usr/bin/env node
// Thin LSP transport over ZtsProject (lib.js holds all the logic so it
// stays unit-testable without a protocol client).

import {
  createConnection,
  ProposedFeatures,
  TextDocumentSyncKind,
} from "vscode-languageserver/node.js";
import { TextDocuments } from "vscode-languageserver";
import { TextDocument } from "vscode-languageserver-textdocument";
import { fileURLToPath, pathToFileURL } from "node:url";
import { ZtsProject } from "./lib.js";
import { format } from "@zestty/native";

const connection = createConnection(ProposedFeatures.all);
const documents = new TextDocuments(TextDocument);

/** @type {ZtsProject} */
let project;

connection.onInitialize((params) => {
  const root = params.workspaceFolders?.[0]
    ? fileURLToPath(params.workspaceFolders[0].uri)
    : process.cwd();
  project = new ZtsProject(root);
  return {
    capabilities: {
      textDocumentSync: TextDocumentSyncKind.Full,
      hoverProvider: true,
      definitionProvider: true,
      completionProvider: {
        triggerCharacters: [".", '"', "'"],
      },
      documentFormattingProvider: true,
    },
  };
});

function refresh(doc) {
  const path = fileURLToPath(doc.uri);
  project.upsert(path, doc.getText(), doc.version);
  connection.sendDiagnostics({
    uri: doc.uri,
    diagnostics: project.diagnostics(path),
  });
}

documents.onDidOpen((e) => refresh(e.document));
documents.onDidChangeContent((e) => refresh(e.document));
documents.onDidClose((e) => {
  project.close(fileURLToPath(e.document.uri));
  connection.sendDiagnostics({ uri: e.document.uri, diagnostics: [] });
});

connection.onHover(({ textDocument, position }) => {
  return project.hover(fileURLToPath(textDocument.uri), position);
});

connection.onCompletion(({ textDocument, position }) => {
  return project.completions(fileURLToPath(textDocument.uri), position);
});

connection.onDocumentFormatting(({ textDocument }) => {
  // zts-fmt (Phase 7): full-document format through the native binding.
  // One whole-document edit — the engine is idempotent, so editors that
  // re-request on save converge immediately. Errors (unparseable
  // buffers) return no edits rather than surfacing a modal: the
  // diagnostics pane already explains WHY it does not parse.
  const doc = documents.get(textDocument.uri);
  if (!doc) return null;
  const text = doc.getText();
  let formatted;
  try {
    formatted = format(text, fileURLToPath(textDocument.uri));
  } catch {
    return null;
  }
  if (formatted == null) return [];
  return [
    {
      range: {
        start: { line: 0, character: 0 },
        end: doc.positionAt(text.length),
      },
      newText: formatted,
    },
  ];
});

connection.onDefinition(({ textDocument, position }) => {
  const defs = project.definitions(fileURLToPath(textDocument.uri), position);
  return defs.map((d) => ({
    uri: pathToFileURL(d.path).toString(),
    range: d.range,
  }));
});

documents.listen(connection);
connection.listen();
