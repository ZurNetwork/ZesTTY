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

connection.onDefinition(({ textDocument, position }) => {
  const defs = project.definitions(fileURLToPath(textDocument.uri), position);
  return defs.map((d) => ({
    uri: pathToFileURL(d.path).toString(),
    range: d.range,
  }));
});

documents.listen(connection);
connection.listen();
