vim.bo.commentstring = "// %s"
vim.bo.shiftwidth = 2
vim.bo.tabstop = 2
vim.bo.expandtab = true

-- Indent (issue #57): tree-sitter's indent engine refuses to compute
-- indentation inside ERROR nodes, and zts constructs (impl blocks above
-- all) ARE error nodes to the reused TypeScript parser — so with
-- nvim-treesitter indent enabled, <CR> inside an impl block resets to
-- column 0. Override with the stock Vim TypeScript indent script:
-- regex-based, tolerant of the superset's extra keywords. Scheduled so
-- it runs AFTER nvim-treesitter's FileType attach sets its own
-- indentexpr (ftplugins source before later-registered FileType
-- autocmds — the schedule is the race fix, not decoration).
local buf = vim.api.nvim_get_current_buf()
vim.schedule(function()
  if not vim.api.nvim_buf_is_valid(buf) then
    return
  end
  vim.api.nvim_buf_call(buf, function()
    -- The runtime script no-ops behind b:did_indent; clear it so the
    -- override applies even if another indent touched the buffer first.
    vim.b[buf].did_indent = nil
    vim.cmd("runtime! indent/typescript.vim")
  end)
end)
