vim.bo.commentstring = "// %s"
vim.bo.shiftwidth = 2
vim.bo.tabstop = 2
vim.bo.expandtab = true

-- Indent (issue #57): same override as ftplugin/zts.lua (see the full
-- rationale there), with the TSX flavor of the stock indent script.
local buf = vim.api.nvim_get_current_buf()
vim.schedule(function()
  if not vim.api.nvim_buf_is_valid(buf) then
    return
  end
  vim.api.nvim_buf_call(buf, function()
    vim.b[buf].did_indent = nil
    vim.cmd("runtime! indent/typescriptreact.vim")
  end)
end)
