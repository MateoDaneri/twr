vim.o.jumpoptions='view'; vim.wo.foldmethod='manual'; vim.wo.foldenable=true
vim.o.laststatus=0; vim.wo.number=false; vim.wo.relativenumber=false; vim.o.scrolloff=0
local lines={} for i=1,30 do lines[i]='synthetic-line-'..i end
vim.api.nvim_buf_set_lines(0,0,-1,false,lines)
vim.cmd('normal! 6G'); vim.cmd('clearjumps'); vim.cmd('normal! G'); vim.cmd('3,10fold')
vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<C-o>',true,false,true),'nx',false)
vim.defer_fn(function()
 vim.cmd('redraw!')
 local result={cursor=vim.api.nvim_win_get_cursor(0), closed=vim.fn.foldclosed(6), screen=vim.fn.screenpos(0,6,1), winline=vim.fn.winline(), view=vim.fn.winsaveview()}
 local f=assert(io.open('/private/tmp/twr-fold-ui-result.json','w')); f:write(vim.json.encode(result)); f:close()
end, 300)
