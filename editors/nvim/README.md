# zestty.nvim

ZesTTY (`.zts` / `.ztsx`) support for Neovim 0.11+ — built for LazyVim,
works anywhere.

- **Highlighting**: the TypeScript/TSX tree-sitter parsers are registered
  for the zts filetypes. zts is a superset, so everything TypeScript
  highlights correctly; the zts constructs themselves (`match` arms, enum
  variant bodies) are error-recovered around and keep their token colors.
- **LSP** (the real DX): wires `@zestty/language-server` through the
  native client — exhaustiveness diagnostics squiggle the original
  `match` as you type, plus hover and go-to-definition, all remapped
  through the compiler's sourcemaps.

## LazyVim / lazy.nvim

With the ZesTTY repo cloned (the plugin lives in a subdirectory, so use a
`dir` spec):

```lua
{
  dir = "~/code/zts/editors/nvim",
  name = "zestty.nvim",
  ft = { "zts", "ztsx" },
  opts = {},
}
```

The tree-sitter TypeScript parsers must be installed (LazyVim's
`nvim-treesitter` defaults include them; otherwise
`:TSInstall typescript tsx`).

## Options

```lua
opts = {
  -- FALLBACK path to @zestty/language-server's server.js, used only when
  -- per-workspace resolution finds nothing. Resolution order, per LSP
  -- root: nearest node_modules/@zestty/language-server walking up from
  -- the workspace root (a consumer repo runs the server it pins — the
  -- same one its CI gate enforces), then the ZesTTY repo layout
  -- (packages/language-server), then this option.
  server_path = nil,
  -- Set false for highlighting-only (no LSP).
  lsp = true,
}
```

## Go-to-definition from consumers (committed twins)

In committed-twins mode, imports from `.ts`/`.svelte` consumers resolve
to the generated `.ts` twin, so go-to-definition is answered by tsserver
— not by `@zestty/language-server` — and would land in generated code
(issue #45). `typescript-zestty-plugin` fixes that inside tsserver
itself: it remaps definition spans from `@generated` twins to the
sibling `.zts` through the `.ts.map` the twin ships with.

Editor-agnostic enablement — install the plugin in the consumer repo and
declare it in `tsconfig.json`; every tsserver-based tool inherits it:

```jsonc
// npm i -D typescript-zestty-plugin
{
  "compilerOptions": {
    "plugins": [{ "name": "typescript-zestty-plugin" }],
  },
}
```

Or globally for Neovim's TypeScript LSP without touching the repo:

```lua
vim.lsp.config("ts_ls", {
  init_options = {
    plugins = {
      {
        name = "typescript-zestty-plugin",
        location = vim.fn.expand("~/code/zts/packages/typescript-plugin"),
        languages = { "typescript" },
      },
    },
  },
})
```

## Indentation

zts constructs (impl blocks above all) are ERROR nodes to the reused
TypeScript tree-sitter parser, and tree-sitter's indent engine bails
inside error nodes — with treesitter indent enabled, `<CR>` inside an
impl block used to reset to column 0 (issue #57). The ftplugins now
override `indentexpr` with the stock Vim TypeScript indent (regex-based,
superset-tolerant), scheduled after nvim-treesitter's attach so the
override wins. No configuration needed.

## Per-construct highlighting: the standing disposition (PR #52)

Every zts syntax change ships with its VS Code grammar update and its
nvim disposition, in the same patch. For nvim the disposition is the same
one every time, and it is recorded here rather than re-argued per
feature: **no per-construct highlight mechanism exists yet.** The reused
TypeScript tree-sitter parser sees zts constructs as bare ERROR
identifier nodes, so there is nothing to attach a capture to — a query
cannot name what the parser did not produce.

Constructs currently in that bucket, i.e. syntactically correct zts that
nvim colours as plain TypeScript or not at all:

- `match` arms and their patterns, enums-with-data, expression `if`,
  `newtype`, `union`, `impl` blocks, `not`, postfix `?`, `constrict`,
  `T[+]` (all pre-0.5.0);
- `lo..=hi` range arm patterns (0.5.0) — the `..=` operator and its
  bounds get no dedicated scope;
- numeric and mixed `union` members (0.5.0) — the members themselves are
  coloured by the TypeScript grammar's own number/string rules, which is
  as good as it gets here, but the `union` keyword still is not.

VS Code has all of these (see `editors/vscode/syntaxes/zts.tmLanguage.json`).
The fix for nvim is not a better query: it is **LS semantic tokens**, on
the post-0.4.0 DX slate. When that lands, this section and the CLAUDE.md
parity caveat both retire — confirm with Zuri at that point.

## Notes

- Filetype registration lives in `ftdetect/zts.lua`, which plugin
  managers source eagerly — so the `ft = { "zts", "ztsx" }` lazy spec
  above works without an `init` block.
- The server is resolved per workspace root when each client starts, so
  a consumer project and the ZesTTY repo itself can run different server
  versions side by side in one Neovim session.
- The LSP activates on the `zts` and `ztsx` filetypes with
  `package.json`/`.git` root markers.
- Svelte `<script lang="zts">` blocks are not injected yet — use
  `zts-check` for those in CI, and keep an eye on the LSP tracking work.

## Formatting

`vim.lsp.buf.format()` works on zts buffers: the language server serves
`textDocument/formatting` backed by zts-fmt (the dprint-fork engine,
Phase 7) — idempotent, zts-aware, line width 80. Wire it to save with
your usual `BufWritePre` autocmd if you want format-on-save.
