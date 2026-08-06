-- zestty.nvim — ZesTTY (zts) support for Neovim 0.11+ / LazyVim.
--
-- What it does:
--  * registers the `zts`/`ztsx` filetypes for `.zts`/`.ztsx` (in ftdetect/,
--    so lazy-loading on `ft` works — issue #34; setup() re-registers only
--    as a harmless idempotent fallback for exotic loaders);
--  * points tree-sitter at the TypeScript/TSX parsers for them (zts is a
--    superset, so highlighting is right for everything except the zts
--    constructs themselves, which the parser error-recovers around);
--  * wires @zestty/language-server through the native LSP client, resolved
--    PER WORKSPACE ROOT (issue #24): a consumer repo runs the server it
--    pins in node_modules (matching its CI gate), while the ZesTTY repo
--    itself runs the repo-HEAD server — version skew between editor and
--    gate becomes structurally impossible.

local M = {}

--- Resolve the language server for one workspace root.
---
--- Order (issue #24): nearest `node_modules/@zestty/language-server` walking
--- up from the root, then the ZesTTY repo layout, then the `server_path`
--- option as the final fallback for layouts we can't guess.
---@param root string
---@param fallback string|nil
---@return string|nil
local function resolve_server(root, fallback)
  local candidates = {
    "node_modules/@zestty/language-server/server.js",
    "packages/language-server/server.js",
  }
  local dir = root
  while dir do
    for _, rel in ipairs(candidates) do
      local p = dir .. "/" .. rel
      if vim.uv.fs_stat(p) then
        return p
      end
    end
    local parent = vim.fs.dirname(dir)
    if parent == dir then
      break
    end
    dir = parent
  end
  if fallback and fallback ~= "" then
    return fallback
  end
  return nil
end

---@class ZesttySetupOpts
---@field server_path string|nil  Fallback path to server.js when no workspace or repo server is found
---@field lsp boolean|nil         Set false to skip LSP wiring (default true)

---@param opts ZesttySetupOpts|nil
function M.setup(opts)
  opts = opts or {}

  -- Idempotent fallback; the authoritative registration is ftdetect/zts.lua.
  vim.filetype.add({
    extension = {
      zts = "zts",
      ztsx = "ztsx",
    },
  })

  -- Tree-sitter: reuse the TS/TSX parsers for the superset.
  vim.treesitter.language.register("typescript", "zts")
  vim.treesitter.language.register("tsx", "ztsx")

  if opts.lsp == false then
    return
  end

  vim.lsp.config("zestty", {
    -- cmd as a function: resolved per client start, so each workspace
    -- root gets ITS server (issue #24). config.root_dir is the root the
    -- client is starting for.
    cmd = function(dispatchers, config)
      local root = (config and config.root_dir) or vim.fn.getcwd()
      local server = resolve_server(root, opts.server_path)
      if not server then
        vim.notify_once(
          "zestty.nvim: @zestty/language-server not found for " .. root
            .. " — highlighting only. Install it in the workspace or pass { server_path = ... }.",
          vim.log.levels.WARN
        )
        error("zestty: no language server for " .. root)
      end
      return vim.lsp.rpc.start({ "node", server, "--stdio" }, dispatchers)
    end,
    filetypes = { "zts", "ztsx" },
    root_markers = { "package.json", ".git" },
  })
  vim.lsp.enable("zestty")
end

return M
