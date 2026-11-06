-- Synthetic reference only. Run with --clean -i NONE -n --headless -l.
vim.o.lines=25; vim.o.laststatus=0; vim.o.cmdheight=1; vim.o.scrolloff=4; vim.o.scroll=11
local lines={} for i=1,120 do lines[i]="  ñ界 "..i end
vim.api.nvim_buf_set_lines(0,0,-1,false,lines)
for _,sequence in ipairs({"<C-d>","3<C-d><C-u>","<C-f>","<C-b>","zt","zz","zb","z<CR>","z.","z-"}) do
 vim.fn.winrestview({topline=40,lnum=50,col=3}); vim.o.scroll=11
 vim.cmd("normal! "..vim.api.nvim_replace_termcodes(sequence,true,false,true))
 print(vim.json.encode({keys=sequence,view=vim.fn.winsaveview(),height=vim.api.nvim_win_get_height(0),scroll=vim.o.scroll}))
end
vim.cmd('qa!')
