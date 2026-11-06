-- Synthetic reference only. Use Neovim --headless --clean -i NONE -n -l
-- with a fresh private HOME and a fresh output path beneath /private/tmp.
local output = assert(vim.env.TWR_PARAGRAPH_OUTPUT)
assert(output:match('^/private/tmp/') and vim.uv.fs_lstat(output) == nil)
local lines = { 'héllo', 'éclair', '', '界world', '', '   ', 'end☕' }
-- Match the installed LazyVim option without loading personal configuration.
vim.opt.virtualedit = 'block'
local modes = { { 'normal', '' }, { 'visual', 'v' }, { 'line', 'V' },
  { 'block', vim.api.nvim_replace_termcodes('<C-v>', true, false, true) } }
local result = { lines = lines, virtualedit = vim.o.virtualedit, cases = {}, version = vim.version().major .. '.' ..
  vim.version().minor .. '.' .. vim.version().patch }
for row = 1, #lines do
  for _, origin in ipairs({ '', 'l' }) do
    for _, mode in ipairs(modes) do
      for _, motion in ipairs({ '}', '{', '2}', '2{' }) do
        vim.cmd('enew!')
        vim.api.nvim_buf_set_lines(0, 0, -1, false, lines)
        vim.fn.setreg('"', '__SENTINEL__', 'v')
        vim.api.nvim_win_set_cursor(0, { row, 0 })
        if origin ~= '' then vim.cmd('silent! normal! ' .. origin) end
        vim.cmd('silent normal! ' .. mode[2] .. motion)
        local finish = vim.api.nvim_win_get_cursor(0)
        local anchor = vim.fn.getpos('v')
        local current_mode = vim.fn.mode()
        if mode[1] ~= 'normal' then vim.cmd('silent normal! y') end
        table.insert(result.cases, { row = row, origin = origin, mode = mode[1],
          motion = motion, finish = finish, anchor = { anchor[2], anchor[3] - 1 },
          observed_mode = current_mode, yank_position = vim.api.nvim_win_get_cursor(0),
          reg = vim.fn.getreg('"'), regtype = vim.fn.getregtype('"') })
      end
    end
  end
end
vim.fn.writefile({ vim.json.encode(result) }, output)
