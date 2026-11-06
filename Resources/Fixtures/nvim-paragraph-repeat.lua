-- Synthetic reference only; use Neovim --headless --clean -i NONE -n -l.
local out = assert(vim.env.TWR_PARAGRAPH_OUTPUT)
assert(out:match('^/private/tmp/') and vim.uv.fs_lstat(out) == nil)
local lines = { 'alpha one', 'alpha two', '', 'beta one', 'beta two', '', 'gamma', '', 'delta' }
local scenarios = {
  { 'd}', 'j', '.' }, { 'd}', 'u' }, { 'd}', 'j', '.', 'u' },
  { 'y}', 'p' }, { 'y}', 'j', 'p' }, { '2d}', 'u' },
  { 'c}XYZ<Esc>', 'j', '.' }, { 'c}XYZ<Esc>', 'u' },
  { 'j', 'c}XYZ<Esc>', 'u' }, { 'l', 'c}XYZ<Esc>', 'u' },
  { 'j', 'j', 'j', 'c{XYZ<Esc>', 'u' }, { '2c}XYZ<Esc>', 'u' },
  { 'c}<Esc>', 'u' }, { 'j', 'j', 'j', 'c{<Esc>', 'u' },
  { 'c}X<CR>Y<Esc>', 'u' },
  { 'G', 'c{XYZ<Esc>', 'u' }, { 'G', 'c}XYZ<Esc>', 'u' },
  { 'j', 'j', 'c}XYZ<Esc>', 'u' },
  { 'c}XYZ<Esc>', 'u', '<C-r>', 'u' }, { 'd}', 'u', '<C-r>', 'u' },
  { 'j', 'c}XYZ<Esc>', 'u', '<C-r>' },
  { 'j', 'j', 'j', 'c{XYZ<Esc>', 'u', '<C-r>' },
  { 'c}<Esc>', 'u', '<C-r>' }, { 'c}X<CR>Y<Esc>', 'u', '<C-r>' },
  { 'j', 'j', 'j', 'd{', '.' }, { 'j', 'j', 'j', 'y{', 'p' },
}
local result = { lines = lines, scenarios = {}, version = vim.version().major .. '.' ..
  vim.version().minor .. '.' .. vim.version().patch }
for _, keys in ipairs(scenarios) do
  vim.cmd('enew!')
  vim.api.nvim_buf_set_lines(0, 0, -1, false, lines)
  -- Flush the fixture write into a separate undo block.
  vim.cmd('let &undolevels = &undolevels')
  vim.api.nvim_win_set_cursor(0, { 1, 0 })
  vim.fn.setreg('"', '__SENTINEL__', 'v')
  local states = {}
  for _, key in ipairs(keys) do
    local sequence = vim.api.nvim_replace_termcodes(key, true, false, true)
    vim.fn.feedkeys(sequence, 'xt')
    table.insert(states, { key = key, pos = vim.api.nvim_win_get_cursor(0),
      lines = vim.api.nvim_buf_get_lines(0, 0, -1, false),
      reg = vim.fn.getreg('"'), regtype = vim.fn.getregtype('"') })
  end
  table.insert(result.scenarios, states)
end
vim.fn.writefile({ vim.json.encode(result) }, out)
