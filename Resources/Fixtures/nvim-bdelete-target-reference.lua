-- Isolated synthetic buffers; no project files are read or written.
local root = '/private/tmp/twr-bdelete-target-reference/'
assert(not vim.uv.fs_lstat(root .. 'a') and not vim.uv.fs_lstat(root .. 'b'))
vim.o.hidden = true
vim.cmd.edit(root .. 'a'); local a = vim.api.nvim_get_current_buf()
vim.cmd.edit(root .. 'b'); local b = vim.api.nvim_get_current_buf()
vim.api.nvim_buf_set_lines(b, 0, -1, false, {'synthetic dirty'})
vim.cmd('buffer ' .. a)
local ok, err = pcall(vim.cmd, 'bdelete ' .. b)
local rejected = {ok = ok, e89 = not ok and tostring(err):find('E89', 1, true) ~= nil,
  current = vim.api.nvim_get_current_buf(), dirty = vim.bo[b].modified, listed = vim.fn.buflisted(b)}
vim.cmd('bdelete! ' .. b)
local hidden = {current = vim.api.nvim_get_current_buf(), alternate = vim.fn.bufnr('#'),
  listed = vim.fn.buflisted(b), loaded = vim.fn.bufloaded(b), exists = vim.fn.bufexists(b)}
vim.cmd('buffer ' .. b); vim.cmd.vsplit(); vim.cmd('buffer ' .. a)
local current_window = vim.api.nvim_get_current_win()
local before = vim.api.nvim_list_wins()
vim.cmd('bdelete ' .. b)
local remaining = vim.api.nvim_list_wins()
print(vim.json.encode({a = a, b = b, rejected = rejected, hidden = hidden,
  visible = {before_count = #before, after_count = #remaining,
    same_window = vim.api.nvim_get_current_win() == current_window,
    current = vim.api.nvim_get_current_buf(), listed = vim.fn.buflisted(b)}}))
vim.cmd('qa!')
