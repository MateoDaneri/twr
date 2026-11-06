-- Synthetic files only; use private HOME, --headless --clean -n -i NONE.
local dir = assert(vim.env.TWR_JUMP_RELOAD_DIR)
assert(dir:match('^/private/tmp/') and vim.uv.fs_stat(dir).type == 'directory')
vim.opt.shada = ''
vim.opt.jumpoptions = 'view'
local function snapshot()
  local entries, index = unpack(vim.fn.getjumplist())
  local jumps = {}
  for _, entry in ipairs(entries) do
    table.insert(jumps, { row = entry.lnum, col = entry.col,
      loaded = vim.fn.bufloaded(entry.bufnr), listed = vim.fn.buflisted(entry.bufnr) })
  end
  return { cursor = vim.api.nvim_win_get_cursor(0), index = index, jumps = jumps }
end
local result = { version = vim.version().major .. '.' .. vim.version().minor .. '.' .. vim.version().patch,
  jumpoptions = vim.o.jumpoptions, cases = {} }
for _, kind in ipairs({ 'unchanged', 'prepend', 'truncate' }) do
  local a, b = dir .. '/' .. kind .. '-a', dir .. '/' .. kind .. '-b'
  assert(vim.uv.fs_lstat(a) == nil and vim.uv.fs_lstat(b) == nil)
  vim.fn.writefile({ 'a1', 'a2', 'a3' }, a); vim.fn.writefile({ 'b1', 'b2', 'b3' }, b)
  vim.cmd('edit! ' .. vim.fn.fnameescape(a))
  vim.api.nvim_win_set_cursor(0, { 2, 1 }); vim.cmd('clearjumps')
  vim.cmd('edit ' .. vim.fn.fnameescape(b)); vim.cmd('bdelete! ' .. vim.fn.bufnr(a))
  if kind == 'prepend' then vim.fn.writefile({ 'x1', 'x2', 'a1', 'a2', 'a3' }, a)
  elseif kind == 'truncate' then vim.fn.writefile({ 'x' }, a) end
  local before = snapshot()
  pcall(vim.fn.feedkeys, vim.api.nvim_replace_termcodes('<C-o>', true, false, true), 'xt')
  table.insert(result.cases, { name = kind, before = before, after = snapshot() })
end
vim.fn.writefile({ vim.json.encode(result) }, dir .. '/reference.json')
vim.cmd('qa!')
