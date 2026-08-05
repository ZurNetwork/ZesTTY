# ZesTTY for VS Code

Language support for **ZesTTY** (`.zts` / `.ztsx`) — a Rust-flavored
superset of TypeScript with compiler-enforced exhaustiveness.

## Current features

- Syntax highlighting: full TypeScript grammar plus zts constructs —
  `match (expr) { ... }` as a control keyword (`.match(...)` method calls
  are left alone), enums-with-data variant and field scopes.
- Language configuration: brackets, comments, auto-closing pairs,
  indentation.

## Install (from source, until it's on the marketplace)

```bash
cd editors/vscode
npx @vscode/vsce package
code --install-extension zestty-*.vsix
```

## Coming next

The LSP proxy (diagnostics, hover, go-to-definition mapped through the
zts sourcemaps) ships as `@zestty/language-server` and will be wired into
this extension.
