-- One synthetic /bin/cat job in clean, isolated Neovim. No project files used.
local path = '/private/tmp/twr-bdelete-terminal-reference-nonexistent'
assert(not vim.uv.fs_lstat(path))
vim.o.hidden = true
vim.cmd.edit(path); local file = vim.api.nvim_get_current_buf()
vim.cmd.enew()
local job = vim.fn.jobstart({'/bin/cat'}, {term = true}); assert(job > 0)
local pid = vim.fn.jobpid(job); assert(pid > 0)
local terminal = vim.api.nvim_get_current_buf()
vim.cmd.vsplit()
local before_count = #vim.api.nvim_list_wins()
local ok, err = pcall(vim.cmd, 'bdelete')
local plain = {ok = ok, e89 = not ok and tostring(err):find('E89', 1, true) ~= nil,
  alive = vim.uv.kill(pid, 0) == 0, listed = vim.fn.buflisted(terminal)}
local forced = pcall(vim.cmd, 'bdelete!')
vim.wait(1000, function() return vim.uv.kill(pid, 0) ~= 0 end, 10)
print(vim.json.encode({before_count = before_count, plain = plain, forced = forced,
  alive_after = vim.uv.kill(pid, 0) == 0, exists = vim.fn.bufexists(terminal),
  listed = vim.fn.buflisted(terminal), current = vim.api.nvim_get_current_buf(),
  file = file, windows_after = #vim.api.nvim_list_wins()}))
vim.cmd('qa!')
