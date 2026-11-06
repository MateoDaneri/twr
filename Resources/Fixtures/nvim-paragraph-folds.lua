-- Synthetic reference: run with --headless --clean -i NONE -n -l, private HOME.
local output = assert(vim.env.TWR_PARAGRAPH_OUTPUT)
assert(output:match('^/private/tmp/') and vim.uv.fs_lstat(output) == nil)
local lines = { 'top', '  alpha', '    deepA', '    deepB', '  omega', '',
  'tail', '  secondA', '  secondB', 'end' }
local sequences = { 'zMj}', 'zMj2}', 'zMj}{', 'zMjd}', 'zMjy}', 'zMjd{',
  'zMjy{', 'zM}d{', 'zM}y{', 'zMjd2}', 'zMjy2}', 'zM}d}', 'zM}y}' }
local result = { lines = lines, cases = {} }
for _, keys in ipairs(sequences) do
  vim.cmd('enew!')
  vim.api.nvim_buf_set_lines(0, 0, -1, false, lines)
  vim.opt_local.foldmethod = 'indent'
  vim.opt_local.shiftwidth = 2
  vim.opt_local.foldminlines = 1
  vim.fn.setreg('"', '__SENTINEL__', 'v')
  vim.api.nvim_win_set_cursor(0, { 1, 0 })
  vim.cmd('silent normal! ' .. keys)
  table.insert(result.cases, { keys = keys, finish = vim.api.nvim_win_get_cursor(0),
    reg = vim.fn.getreg('"'), regtype = vim.fn.getregtype('"'),
    lines = vim.api.nvim_buf_get_lines(0, 0, -1, false) })
end
vim.fn.writefile({ vim.json.encode(result) }, output)
