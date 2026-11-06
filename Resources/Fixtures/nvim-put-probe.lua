-- Synthetic in-memory reference; run only in a clean standalone Neovim.
for _,case in ipairs({{"vll", "v", "XYZ", "p"},{"vll", "v", "XYZ", "P"},{"vll", "V", "one", "p"},{"v$", "V", "one", "p"},{"V", "v", "XYZ", "p"}}) do
vim.api.nvim_buf_set_lines(0,0,-1,false,{"abcdefghij","next"})
vim.fn.setreg('a',case[3],case[2]);vim.fn.setreg('"','OLD','v'); vim.api.nvim_win_set_cursor(0,{1,2})
vim.cmd('normal! '..case[1]..'"a'..case[4])
print(vim.json.encode({case=case,lines=vim.api.nvim_buf_get_lines(0,0,-1,false),cursor=vim.api.nvim_win_get_cursor(0),unnamed=vim.fn.getreg('"')}))
end vim.cmd('qa!')
