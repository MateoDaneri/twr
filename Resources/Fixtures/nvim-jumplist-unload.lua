-- Synthetic files only; run with private HOME and --headless --clean -n -i NONE.
local dir = assert(vim.env.TWR_JUMP_UNLOAD_DIR)
assert(dir:match('^/private/tmp/') and vim.uv.fs_stat(dir).type == 'directory')
local a, b = dir .. '/a.txt', dir .. '/b.txt'
assert(vim.uv.fs_lstat(a) == nil and vim.uv.fs_lstat(b) == nil)
vim.fn.writefile({ 'a1', 'a2', 'a3' }, a)
vim.fn.writefile({ 'b1', 'b2', 'b3' }, b)
vim.opt.shada = ''
vim.opt.jumpoptions = 'view'
local function snapshot()
  local entries, index = unpack(vim.fn.getjumplist())
  local jumps = {}
  for _, entry in ipairs(entries) do
    table.insert(jumps, { name = vim.fn.fnamemodify(vim.fn.bufname(entry.bufnr), ':t'),
      row = entry.lnum, col = entry.col, exists = vim.fn.bufexists(entry.bufnr),
      loaded = vim.fn.bufloaded(entry.bufnr), listed = vim.fn.buflisted(entry.bufnr) })
  end
  return { buffer = vim.fn.expand('%:t'), cursor = vim.api.nvim_win_get_cursor(0), index = index, jumps = jumps }
end
local result = { version = vim.version().major .. '.' .. vim.version().minor .. '.' .. vim.version().patch,
  jumpoptions = vim.o.jumpoptions, cases = {} }
for _, kind in ipairs({ 'none', 'bunload', 'bdelete', 'bwipeout' }) do
  vim.cmd('silent! only!')
  vim.cmd('edit! ' .. vim.fn.fnameescape(a))
  vim.api.nvim_win_set_cursor(0, { 2, 1 })
  vim.cmd('clearjumps')
  vim.cmd('edit ' .. vim.fn.fnameescape(b))
  local id = vim.fn.bufnr(a)
  if kind ~= 'none' then vim.cmd(kind .. '! ' .. id) end
  local before = snapshot()
  pcall(vim.fn.feedkeys, vim.api.nvim_replace_termcodes('<C-o>', true, false, true), 'xt')
  table.insert(result.cases, { name = kind, before = before, after = snapshot() })
end
vim.fn.writefile({ vim.json.encode(result) }, dir .. '/reference.json')
vim.cmd('qa!')
