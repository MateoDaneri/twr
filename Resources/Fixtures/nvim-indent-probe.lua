-- Synthetic reference only. Run with --clean -i NONE -n --headless -l.
vim.opt.expandtab = true
vim.opt.shiftwidth = 2
vim.opt.tabstop = 2
vim.opt.shiftround = true
vim.opt.smartindent = true
vim.opt.autoindent = true
for _, case in ipairs({
  {"tab", {"abc"}, "i<Tab>X<Esc>"},
  {"tab-backspace", {"abc"}, "i<Tab><BS>X<Esc>"},
  {"split", {"  abc"}, "A<CR>X<Esc>"},
  {"open", {"  abc"}, "oX<Esc>"},
  {"above", {"  abc"}, "OX<Esc>"},
  {"shift", {" a", "   b", "", "c"}, "3>>"},
  {"visual-shift", {" a", "   b", "", "c"}, "Vj>"},
  {"unshift", {"   a"}, "<<"},
}) do
  vim.api.nvim_buf_set_lines(0, 0, -1, false, case[2])
  vim.api.nvim_win_set_cursor(0, {1, 0})
  vim.cmd.normal({vim.api.nvim_replace_termcodes(case[3], true, false, true), bang=true})
  print(vim.json.encode({case=case[1], lines=vim.api.nvim_buf_get_lines(0,0,-1,false), cursor=vim.api.nvim_win_get_cursor(0)}))
end
vim.cmd('qa!')
