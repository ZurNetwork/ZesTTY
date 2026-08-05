-- zestty.nvim — ZesTTY (zts) support for Neovim 0.11+ / LazyVim.
--
-- What it does:
--  * registers the `zts`/`ztsx` filetypes for `.zts`/`.ztsx`;
--  * points tree-sitter at the TypeScript/TSX parsers for them (zts is a
--    superset, so highlighting is right for everything except the zts
--    constructs themselves, which the parser error-recovers around);
--  * wires @zestty/language-server through the native LSP client, which
--    is where the real DX lives: exhaustiveness diagnostics on the
--    original `match`, hover, go-to-definition.

local M = {}

--- Find the language server: explicit option, workspace node_modules,
--- or the ZesTTY repo layout.
---@param explicit string|nil
---@return string|nil
local function find_server(explicit)
  if explicit and explicit ~= "" then
    return explicit
  end
  local candidates = {
    "node_modules/@zestty/language-server/server.js",
    "packages/language-server/server.js",
  }
  local dir = vim.fn.getcwd()
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
  return nil
end

---@class ZesttySetupOpts
---@field server_path string|nil  Absolute path to server.js (auto-detected when nil)
---@field lsp boolean|nil         Set false to skip LSP wiring (default true)

---@param opts ZesttySetupOpts|nil
function M.setup(opts)
  opts = opts or {}

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

  local server = find_server(opts.server_path)
  if not server then
    vim.notify_once(
      "zestty.nvim: @zestty/language-server not found — highlighting only. "
        .. "Install it in the workspace or pass { server_path = ... }.",
      vim.log.levels.WARN
    )
    return
  end

  vim.lsp.config("zestty", {
    cmd = { "node", server, "--stdio" },
    filetypes = { "zts", "ztsx" },
    root_markers = { "package.json", ".git" },
  })
  vim.lsp.enable("zestty")
end

return M
