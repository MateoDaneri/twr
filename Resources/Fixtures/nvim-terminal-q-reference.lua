-- Run with nvim --headless --clean -n -i NONE -l; synthetic buffers only.
vim.opt.hidden = true
vim.cmd('enew')
local file = vim.api.nvim_get_current_buf()
vim.api.nvim_buf_set_lines(file, 0, -1, false, {'synthetic file'})
vim.cmd('vnew')
vim.cmd('terminal /bin/cat')
local terminal = vim.api.nvim_get_current_buf()
local job = vim.b.terminal_job_id
-- The buffer-local mapping is copied from config/autocmds.lua, without
-- loading the user's configuration, projects, history, or plugins.
vim.keymap.set('n', 'q', '<cmd>close<cr>', {buffer = terminal, silent = true})
vim.cmd('stopinsert')
local before = #vim.api.nvim_list_wins()
vim.api.nvim_feedkeys('q', 'xt', false)
local split = {
  before = before,
  after = #vim.api.nvim_list_wins(),
  returned_to_file = vim.api.nvim_get_current_buf() == file,
  terminal_loaded = vim.fn.bufloaded(terminal) == 1,
  terminal_listed = vim.fn.buflisted(terminal) == 1,
  job_alive = vim.fn.jobwait({job}, 0)[1] == -1,
}
vim.fn.jobstop(job)
vim.cmd('enew!')
vim.cmd('terminal /bin/cat')
terminal = vim.api.nvim_get_current_buf()
job = vim.b.terminal_job_id
vim.keymap.set('n', 'q', '<cmd>close<cr>', {buffer = terminal, silent = true})
vim.cmd('stopinsert')
vim.v.errmsg = ''
vim.api.nvim_feedkeys('q', 'xt', false)
local last = {
  windows = #vim.api.nvim_list_wins(),
  e444 = vim.v.errmsg:find('E444', 1, true) ~= nil,
  same_buffer = vim.api.nvim_get_current_buf() == terminal,
  job_alive = vim.fn.jobwait({job}, 0)[1] == -1,
}
print(vim.json.encode({split = split, last = last}))
vim.fn.jobstop(job)
vim.cmd('qa!')
