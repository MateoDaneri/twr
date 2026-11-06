-- Run with --headless --clean -n -i NONE -l and a fresh private HOME.
local dir = assert(vim.env.TWR_JUMP_SEARCH_DIR)
assert(dir:match('^/private/tmp/') and vim.uv.fs_stat(dir).type == 'directory')
vim.opt.shada = ''
vim.opt.jumpoptions = 'view'
local function feed(keys)
  vim.fn.feedkeys(vim.api.nvim_replace_termcodes(keys, true, false, true), 'xt')
end
local function snapshot()
  local entries, index = unpack(vim.fn.getjumplist())
  local jumps = {}
  for _, entry in ipairs(entries) do
    table.insert(jumps, { row = entry.lnum, col = entry.col })
  end
  return { cursor = vim.api.nvim_win_get_cursor(0), jumps = jumps, index = index }
end
local cases = {
  { 'forward', '/needle<CR>', 1 }, { 'backward', '?needle<CR>', 4 },
  { 'missing', '/missing<CR>', 1 }, { 'cancel', '/needle<Esc>', 1 },
  { 'next', 'n', 1 }, { 'previous', 'N', 4 }, { 'word', '*', 2 },
  { 'wrap', '/needle<CR>', 2 }, { 'count', '2/needle<CR>', 1 },
}
local result = { version = vim.version().major .. '.' .. vim.version().minor .. '.' .. vim.version().patch,
  jumpoptions = vim.o.jumpoptions, cases = {} }
for _, case in ipairs(cases) do
  vim.cmd('enew!')
  vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'first', 'needle one', 'middle', 'needle two', 'last' })
  vim.api.nvim_win_set_cursor(0, { case[3], 0 })
  vim.cmd('clearjumps')
  vim.fn.setreg('/', 'needle')
  local before = snapshot()
  pcall(feed, case[2])
  table.insert(result.cases, { name = case[1], before = before, after = snapshot() })
end
vim.fn.writefile({ vim.json.encode(result) }, dir .. '/reference.json')
vim.cmd('qa!')
