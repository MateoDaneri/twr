-- Synthetic paths only. Run --headless --clean -n -i NONE -l with private HOME.
local dir = assert(vim.env.TWR_JUMP_BUFFER_DIR)
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
    table.insert(jumps, { name = vim.fn.fnamemodify(vim.fn.bufname(entry.bufnr), ':t'), row = entry.lnum, col = entry.col })
  end
  return { buffer = vim.fn.expand('%:t'), cursor = vim.api.nvim_win_get_cursor(0), index = index, jumps = jumps }
end
local result = { version = vim.version().major .. '.' .. vim.version().minor .. '.' .. vim.version().patch,
  jumpoptions = vim.o.jumpoptions, cases = {} }
for _, kind in ipairs({ 'edit', 'buffer', 'alternate', 'split', 'same' }) do
  vim.cmd('silent! only!')
  vim.cmd('edit! ' .. vim.fn.fnameescape(a))
  local b_id = vim.fn.bufadd(b)
  vim.api.nvim_win_set_cursor(0, { 2, 1 })
  vim.cmd('clearjumps')
  if kind == 'edit' then vim.cmd('edit ' .. vim.fn.fnameescape(b))
  elseif kind == 'buffer' then vim.cmd('buffer ' .. b_id)
  elseif kind == 'alternate' then
    vim.cmd('edit ' .. vim.fn.fnameescape(b)); vim.cmd('edit ' .. vim.fn.fnameescape(a))
    vim.cmd('clearjumps'); vim.cmd('buffer #')
  elseif kind == 'split' then vim.cmd('split ' .. vim.fn.fnameescape(b))
  else vim.cmd('edit ' .. vim.fn.fnameescape(a)) end
  table.insert(result.cases, { name = kind, after = snapshot() })
end
vim.fn.writefile({ vim.json.encode(result) }, dir .. '/reference.json')
vim.cmd('qa!')
