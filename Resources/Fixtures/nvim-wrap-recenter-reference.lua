local lines = {}
for i=1,30 do lines[i]=('line%02d '):format(i)..string.rep('x',(i%3)*30) end
vim.api.nvim_buf_set_lines(0,0,-1,false,lines)
local out={}
for _,n in ipairs({1,3,6,15,29}) do
  for _,k in ipairs({'zt','zz','zb'}) do
    vim.cmd('normal! gg0')
    vim.api.nvim_win_set_cursor(0,{n,0})
    vim.cmd('redraw')
    local before=vim.fn.winsaveview()
    vim.cmd('normal! '..k)
    vim.cmd('redraw')
    local after=vim.fn.winsaveview()
    out[#out+1]={line=n,key=k,before={top=before.topline,skip=before.skipcol},after={top=after.topline,skip=after.skipcol},cursor=vim.api.nvim_win_get_cursor(0),row=vim.fn.winline()}
  end
end
print(vim.json.encode(out))
vim.cmd('qa!')
