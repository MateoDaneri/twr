vim.opt.shada = ''
vim.opt.jumpoptions = 'view'
local lines = {}
for i = 1, 40 do lines[i] = tostring(i) end
vim.api.nvim_buf_set_lines(0, 0, -1, false, lines)
vim.api.nvim_win_set_cursor(0, { 2, 0 })
vim.cmd('clearjumps')
vim.cmd('normal! G')
local first = vim.api.nvim_get_current_win()
vim.cmd('vsplit')
local second = vim.api.nvim_get_current_win()
local function state(window)
  return vim.api.nvim_win_call(window, function()
    local entries, index = unpack(vim.fn.getjumplist())
    return { count = #entries, index = index }
  end)
end
local before = { first = state(first), second = state(second) }
vim.cmd('clearjumps')
local after = { first = state(first), second = state(second) }
local aliases = {}
for _, command in ipairs({ 'clearj', 'clearju', 'clearjum', 'clearjump', 'clearjumps', 'clearjumps!' }) do
  local ok, parsed = pcall(vim.api.nvim_parse_cmd, command, {})
  aliases[command] = ok and parsed.cmd or false
end
vim.api.nvim_out_write(vim.json.encode({ before = before, after = after, aliases = aliases }) .. '\n')
vim.cmd('qa!')
