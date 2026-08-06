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
