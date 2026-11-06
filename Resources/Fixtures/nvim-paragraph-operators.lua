-- Synthetic only. Run with Neovim --headless --clean -i NONE -n -l and a
-- private HOME. TWR_PARAGRAPH_OUTPUT must point to a fresh temporary file.
local output = assert(vim.env.TWR_PARAGRAPH_OUTPUT)
assert(output:match('^/private/tmp/'), 'output must be under /private/tmp')
assert(vim.uv.fs_lstat(output) == nil, 'refusing to overwrite an existing file')
vim.opt.autoindent = true
local lines = { '  alpha', 'bravo', '', 'charlie', '', '', 'delta' }
local result = { version = vim.version().major .. '.' .. vim.version().minor .. '.' .. vim.version().patch,
  autoindent = vim.o.autoindent, lines = lines, cases = {}, changes = {} }
for row = 1, #lines do
  for _, col in ipairs({ 0, math.min(2, #lines[row]) }) do
    for _, keys in ipairs({ 'y}', 'y{', 'd}', 'd{', '2y}', 'y2}', '2d{', 'd2{' }) do
      vim.api.nvim_buf_set_lines(0, 0, -1, false, lines)
      vim.fn.setreg('"', '__SENTINEL__', 'v')
      vim.api.nvim_win_set_cursor(0, { row, col })
      local start = vim.api.nvim_win_get_cursor(0)
      vim.cmd('silent normal! ' .. keys)
      table.insert(result.cases, { keys = keys, start = start,
        finish = vim.api.nvim_win_get_cursor(0), reg = vim.fn.getreg('"'),
        regtype = vim.fn.getregtype('"'), lines = vim.api.nvim_buf_get_lines(0, 0, -1, false) })
    end
  end
end
for row = 1, #lines do
  for _, col in ipairs({ 0, math.min(2, #lines[row]) }) do
    for _, keys in ipairs({ 'c}', 'c{', '2c}', 'c2{' }) do
      vim.api.nvim_buf_set_lines(0, 0, -1, false, lines)
      vim.fn.setreg('"', '__SENTINEL__', 'v')
      vim.api.nvim_win_set_cursor(0, { row, col })
      local start = vim.api.nvim_win_get_cursor(0)
      vim.cmd('silent normal! ' .. keys .. 'X' .. vim.api.nvim_replace_termcodes('<Esc>', true, false, true))
      table.insert(result.changes, { keys = keys, start = start,
        finish = vim.api.nvim_win_get_cursor(0), reg = vim.fn.getreg('"'),
        regtype = vim.fn.getregtype('"'), lines = vim.api.nvim_buf_get_lines(0, 0, -1, false) })
    end
  end
end
vim.fn.writefile({ vim.json.encode(result) }, output)
