-- Clean, isolated Neovim reference. No synthetic file is created.
local root = '/private/tmp/twr-bdelete-layout-reference/'
assert(not vim.uv.fs_lstat(root .. 'a') and not vim.uv.fs_lstat(root .. 'b'))
vim.o.hidden = true
vim.cmd.edit(root .. 'a')
local a = vim.api.nvim_get_current_buf()
local case = assert(vim.env.TWR_CASE)
if case == 'all' then
  vim.cmd.edit(root .. 'b'); vim.cmd.vsplit(); vim.cmd.split()
elseif case == 'mixed' then
  vim.cmd.vsplit(); vim.cmd.edit(root .. 'b'); vim.cmd.split()
elseif case == 'one' then
  vim.cmd.vsplit(); vim.cmd.edit(root .. 'b')
else
  error('unknown synthetic case')
end
local b = vim.api.nvim_get_current_buf()
local before = vim.api.nvim_list_wins()
local function index(win)
  for i, id in ipairs(before) do if id == win then return i - 1 end end
  error('unexpected window')
end
local buffers = {}
for _, win in ipairs(before) do
  table.insert(buffers, vim.api.nvim_win_get_buf(win) == b and 'b' or 'a')
end
vim.cmd('bdelete!')
local remaining = {}
for _, win in ipairs(vim.api.nvim_list_wins()) do table.insert(remaining, index(win)) end
print(vim.json.encode({case = case, before = buffers, remaining = remaining,
  active = index(vim.api.nvim_get_current_win()), current = vim.api.nvim_get_current_buf() == a and 'a' or 'other',
  listed = vim.fn.buflisted(b), loaded = vim.fn.bufloaded(b)}))
vim.cmd('qa!')
