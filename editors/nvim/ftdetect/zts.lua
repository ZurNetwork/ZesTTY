-- Filetype registration lives HERE, not in setup(): plugin managers source
-- ftdetect/ eagerly even for lazy-loaded plugins, so `ft = { "zts", "ztsx" }`
-- specs work. Inside setup() this deadlocks: the FileType event can't fire
-- until the mapping exists, and the mapping doesn't exist until the plugin
-- loads on the FileType event (issue #34).
vim.filetype.add({
  extension = {
    zts = "zts",
    ztsx = "ztsx",
  },
})
