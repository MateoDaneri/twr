-- Synthetic only. Use Neovim --headless --clean -i NONE -n -l with a fresh
-- private HOME and TWR_JUMP_DIR created under /private/tmp by the caller.
local dir = assert(vim.env.TWR_JUMP_DIR)
assert(dir:match('^/private/tmp/') and vim.uv.fs_stat(dir).type == 'directory')
local a, b = dir .. '/alpha.txt', dir .. '/beta.txt'
assert(vim.uv.fs_lstat(a) == nil and vim.uv.fs_lstat(b) == nil)
vim.fn.writefile({ 'a1', 'a2', '', 'a4', 'a5', '', 'a7', 'a8' }, a)
vim.fn.writefile({ 'b1', 'b2', '', 'b4', '', 'b6' }, b)
local long = dir .. '/long.txt'
assert(vim.uv.fs_lstat(long) == nil)
local long_lines = {}
for i = 1, 130 do table.insert(long_lines, 'line ' .. i) end
vim.fn.writefile(long_lines, long)
vim.opt.jumpoptions = 'view' -- Matches the installed LazyVim option.
vim.opt.shada = ''
local result = { version = vim.version().major .. '.' .. vim.version().minor .. '.' ..
  vim.version().patch, jumpoptions = vim.o.jumpoptions, scenarios = {}, unobserved = {}, edits = {}, capacity = {}, views = {}, removed = {}, view_edits = {}, columns = {}, typed_columns = {}, typed_lines = {}, typed_operators = {}, typed_changes = {} }
local function snapshot(action)
  local list, index = unpack(vim.fn.getjumplist())
  local entries = {}
  for _, entry in ipairs(list) do
    table.insert(entries, { buffer = vim.fn.fnamemodify(vim.fn.bufname(entry.bufnr), ':t'),
      row = entry.lnum, col = entry.col })
  end
  local v = vim.fn.winsaveview()
  return { action = action, buffer = vim.fn.expand('%:t'), cursor = vim.api.nvim_win_get_cursor(0),
    index = index, jumps = entries, view = { v.topline, v.leftcol, v.skipcol } }
end
local function reset()
  vim.cmd('silent! only!')
  vim.cmd('edit! ' .. vim.fn.fnameescape(a))
  vim.api.nvim_win_set_cursor(0, { 1, 0 })
  vim.cmd('clearjumps')
end
local function keys(s)
  vim.fn.feedkeys(vim.api.nvim_replace_termcodes(s, true, false, true), 'xt')
end
local function run(actions)
  reset()
  local states = { snapshot('start') }
  for _, action in ipairs(actions) do
    if action == 'edit B' then vim.cmd('edit ' .. vim.fn.fnameescape(b))
    elseif action == 'edit A' then vim.cmd('edit ' .. vim.fn.fnameescape(a))
    else keys(action) end
    table.insert(states, snapshot(action))
  end
  table.insert(result.scenarios, states)
end
run({ '}', '}', '<C-o>', '<C-o>', '<C-i>', '<C-i>', '<C-i>' })
run({ 'G', 'gg', 'G', '<C-o>', '<C-o>', '<C-i>', '}', '<C-o>', '<C-i>' })
run({ '}', 'edit B', '}', 'G', '<C-o>', '<C-o>', '<C-o>', '<C-i>', '<C-i>' })
run({ '}', '}', '2<C-o>', '2<C-i>', '3<C-o>', '<C-i>' })
run({ '}', '9<C-o>', '<C-o>', '<C-i>', '9<C-i>' })
reset()
local states = { snapshot('start') }
for _, key in ipairs({ '}', '}', 'G' }) do keys(key); table.insert(states, snapshot(key)) end
local original = vim.api.nvim_get_current_win()
vim.cmd('vsplit')
local split = vim.api.nvim_get_current_win()
table.insert(states, snapshot('split'))
keys('<C-o>'); table.insert(states, snapshot('split Ctrl-o'))
vim.api.nvim_set_current_win(original); table.insert(states, snapshot('original focus'))
keys('<C-o>'); table.insert(states, snapshot('original Ctrl-o'))
vim.api.nvim_set_current_win(split); table.insert(states, snapshot('split focus'))
keys('<C-i>'); table.insert(states, snapshot('split Ctrl-i'))
table.insert(result.scenarios, states)
-- getjumplist() cleans duplicates, so do not call it between these inputs.
-- Record only cursor/view until the final snapshot, to avoid observer effects.
for _, sequence in ipairs({
  { '}', '}', '<C-o>', '<C-o>', '<C-i>' },
  { 'G', 'gg', 'G', '<C-o>', '<C-i>', '}' },
  { '}', 'edit B', '}', 'G', '<C-o>', '<C-o>', '<C-o>' },
  { '}', '9<C-o>', '<C-o>', '<C-i>' },
}) do
  reset()
  local trace = {}
  local function light(action)
    local v = vim.fn.winsaveview()
    table.insert(trace, { action = action, buffer = vim.fn.expand('%:t'),
      cursor = vim.api.nvim_win_get_cursor(0), view = { v.topline, v.leftcol, v.skipcol } })
  end
  light('start')
  for _, action in ipairs(sequence) do
    if action == 'edit B' then vim.cmd('edit ' .. vim.fn.fnameescape(b)) else keys(action) end
    light(action)
  end
  table.insert(result.unobserved, { trace = trace, final = snapshot('final') })
end
for _, edit in ipairs({ 'insert before', 'delete before', 'delete marked', 'delete marked column', 'delete to EOF', 'replace marked', 'replace same count', 'replace more' }) do
  reset()
  keys('4G'); if edit == 'delete marked column' then keys('$') end; keys('7G')
  local before = snapshot('before')
  if edit == 'insert before' then vim.api.nvim_buf_set_lines(0, 0, 0, false, { 'new1', 'new2' })
  elseif edit == 'delete before' then vim.api.nvim_buf_set_lines(0, 1, 3, false, {})
  elseif edit == 'delete marked' or edit == 'delete marked column' then vim.api.nvim_buf_set_lines(0, 2, 5, false, {})
  elseif edit == 'delete to EOF' then vim.api.nvim_buf_set_lines(0, 2, -1, false, {})
  elseif edit == 'replace marked' then vim.api.nvim_buf_set_lines(0, 2, 5, false, { 'replacement' })
  elseif edit == 'replace same count' then vim.api.nvim_buf_set_lines(0, 2, 5, false, { 'x', 'y', 'z' })
  else vim.api.nvim_buf_set_lines(0, 2, 5, false, { 'w', 'x', 'y', 'z' }) end
  table.insert(result.edits, { edit = edit, before = before, after = snapshot('after') })
end
for _, edit in ipairs({ 'insert before', 'insert at mark', 'delete before', 'delete mark', 'newline before', 'newline at mark' }) do
  reset(); keys('4G'); keys('$'); keys('7G')
  local before = snapshot('before')
  if edit == 'insert before' then vim.api.nvim_buf_set_text(0, 3, 0, 3, 0, { 'XYZ' })
  elseif edit == 'insert at mark' then vim.api.nvim_buf_set_text(0, 3, 1, 3, 1, { 'XYZ' })
  elseif edit == 'delete before' then vim.api.nvim_buf_set_text(0, 3, 0, 3, 1, { '' })
  elseif edit == 'delete mark' then vim.api.nvim_buf_set_text(0, 3, 1, 3, 2, { '' })
  elseif edit == 'newline before' then vim.api.nvim_buf_set_text(0, 3, 0, 3, 0, { '', '' })
  else vim.api.nvim_buf_set_text(0, 3, 1, 3, 1, { '', '' }) end
  table.insert(result.columns, { edit = edit, before = before, after = snapshot('after') })
end
for _, edit in ipairs({ '0iXYZ<Esc>', 'liXYZ<Esc>', '0x', 'lx', '0i<CR><Esc>', 'li<CR><Esc>' }) do
  reset(); keys('4G'); keys('$'); keys('7G')
  local before = snapshot('before')
  vim.api.nvim_win_set_cursor(0, { 4, 0 })
  keys(edit)
  vim.api.nvim_win_set_cursor(0, { vim.api.nvim_buf_line_count(0), 0 })
  table.insert(result.typed_columns, { edit = edit, before = before, after = snapshot('after') })
end
for _, edit in ipairs({ { 3, 'o<Esc>' }, { 4, 'O<Esc>' }, { 4, '0i<CR><Esc>' },
  { 4, 'dd' }, { 3, '2dd' }, { 2, 'dd' } }) do
  reset(); keys('4G'); keys('$'); keys('7G')
  local before = snapshot('before')
  vim.api.nvim_win_set_cursor(0, { edit[1], 0 })
  keys(edit[2])
  vim.api.nvim_win_set_cursor(0, { vim.api.nvim_buf_line_count(0), 0 })
  table.insert(result.typed_lines, { row = edit[1], edit = edit[2], before = before, after = snapshot('after') })
end
for _, edit in ipairs({ { 1, 'd}' }, { 2, '3dd' }, { 7, 'd{' }, { 3, 'd}' } }) do
  reset(); keys('4G'); keys('$'); keys('7G')
  local before = snapshot('before')
  vim.api.nvim_win_set_cursor(0, { edit[1], 0 })
  keys(edit[2])
  vim.api.nvim_win_set_cursor(0, { vim.api.nvim_buf_line_count(0), 0 })
  table.insert(result.typed_operators, { row = edit[1], edit = edit[2], before = before, after = snapshot('after') })
end
for _, edit in ipairs({ { 4, 1, 'c}X<Esc>' }, { 2, 1, 'c}X<Esc>' },
  { 1, 1, 'c}X<Esc>' }, { 4, 3, 'c}X<Esc>' }, { 4, 7, 'c{X<Esc>' },
  { 4, 1, 'c}X<CR>Y<Esc>' } }) do
  reset(); keys(edit[1] .. 'G'); keys('$'); keys('7G')
  local before = snapshot('before')
  vim.api.nvim_win_set_cursor(0, { edit[2], 0 })
  keys(edit[3])
  vim.api.nvim_win_set_cursor(0, { vim.api.nvim_buf_line_count(0), 0 })
  table.insert(result.typed_changes, { mark = edit[1], row = edit[2], edit = edit[3],
    before = before, after = snapshot('after') })
end
vim.cmd('silent! only!')
vim.cmd('edit! ' .. vim.fn.fnameescape(long))
vim.api.nvim_win_set_cursor(0, { 1, 0 })
vim.cmd('clearjumps')
for i = 2, 112 do keys(i .. 'G') end
table.insert(result.capacity, snapshot('full'))
keys('100<C-o>'); table.insert(result.capacity, snapshot('100<C-o>'))
keys('99<C-o>'); table.insert(result.capacity, snapshot('99<C-o>'))
vim.o.lines = 12
vim.cmd('edit! ' .. vim.fn.fnameescape(long))
vim.api.nvim_win_set_cursor(0, { 1, 0 })
vim.cmd('clearjumps')
table.insert(result.views, snapshot('start'))
for _, action in ipairs({ '30G', 'zt', '70G', 'zt', '<C-o>', '<C-i>' }) do
  keys(action); table.insert(result.views, snapshot(action))
end
vim.cmd('edit! ' .. vim.fn.fnameescape(long))
vim.api.nvim_win_set_cursor(0, { 1, 0 })
vim.cmd('clearjumps')
for _, action in ipairs({ '30G', 'zt', '70G', 'zt' }) do keys(action) end
table.insert(result.view_edits, snapshot('before insertion'))
vim.api.nvim_buf_set_lines(0, 0, 0, false, { 'inserted one', 'inserted two' })
table.insert(result.view_edits, snapshot('after insertion'))
keys('<C-o>'); table.insert(result.view_edits, snapshot('return'))
reset()
keys('}')
vim.cmd('edit ' .. vim.fn.fnameescape(b))
keys('}')
vim.cmd('edit ' .. vim.fn.fnameescape(a))
table.insert(result.removed, snapshot('before wipe'))
local beta = vim.fn.bufnr(b)
vim.cmd('bwipeout! ' .. beta)
table.insert(result.removed, snapshot('after wipe'))
for _, action in ipairs({ '<C-o>', '<C-o>', '<C-i>' }) do
  keys(action); table.insert(result.removed, snapshot(action))
end
vim.fn.writefile({ vim.json.encode(result) }, dir .. '/reference.json')
