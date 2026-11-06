vim.opt.jumpoptions = 'view'
vim.opt.shada = ''
vim.opt.hidden = true
local function new_term(label)
  vim.cmd('enew')
  local buffer = vim.api.nvim_get_current_buf()
  assert(vim.fn.termopen({'/bin/sh', '-c', 'printf "ONE\\nTWO\\nTHREE\\nFOUR\\n"; sleep 2'}) > 0)
  assert(vim.wait(1000, function()
    return table.concat(vim.api.nvim_buf_get_lines(buffer, 0, -1, false), '\n'):find('TWO', 1, true) ~= nil
  end, 10), label)
  return buffer
end
local first = new_term('first')
local second
vim.api.nvim_win_set_cursor(0, {2, 1})
vim.cmd('clearjumps')
local function snapshot()
  local entries, index = unpack(vim.fn.getjumplist())
  local marks = {}
  for _, entry in ipairs(entries) do
    table.insert(marks, {buffer = entry.bufnr == first and 'first' or entry.bufnr == second and 'second' or 'other', row = entry.lnum, col = entry.col})
  end
  local current = vim.api.nvim_get_current_buf()
  return {buffer = current == first and 'first' or current == second and 'second' or 'other', cursor = vim.api.nvim_win_get_cursor(0), index = index, marks = marks}
end
local result = {before = snapshot()}
second = new_term('second')
vim.api.nvim_win_set_cursor(0, {3, 2})
result.second = snapshot()
local function key(k)
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes(k, true, false, true), 'nx', false)
end
key('<C-o>'); result.back = snapshot()
key('<C-i>'); result.forward = snapshot()
vim.cmd('buffer ' .. first); result.explicit_first = snapshot()
key('<C-o>'); result.back_after_buffer = snapshot()
vim.api.nvim_out_write(vim.json.encode(result) .. '\n')
vim.cmd('qa!')
