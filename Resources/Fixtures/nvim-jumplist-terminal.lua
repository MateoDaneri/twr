vim.opt.jumpoptions='view'; vim.opt.shada=''
vim.cmd('enew!')
local terminal=vim.api.nvim_get_current_buf()
local channel=vim.fn.termopen({'/bin/sh','-c','printf "ONE\\nTWO\\nTHREE\\nFOUR\\nFIVE\\n"; sleep 2'})
assert(channel > 0)
assert(vim.wait(1000, function() return table.concat(vim.api.nvim_buf_get_lines(terminal, 0, -1, false), '\n'):find('TWO', 1, true) ~= nil end, 20))
vim.api.nvim_win_set_cursor(0, {2, 1})
vim.cmd('clearjumps')
local function snapshot()
 local entries, index=unpack(vim.fn.getjumplist()); local marks={}
 for _,e in ipairs(entries) do table.insert(marks,{term=e.bufnr==terminal,row=e.lnum,col=e.col}) end
 return {terminal=vim.api.nvim_get_current_buf()==terminal,cursor=vim.api.nvim_win_get_cursor(0),mode=vim.fn.mode(1),index=index,jumps=marks}
end
local before=snapshot()
local path='/private/tmp/twr-no-such-terminal-jump-file'
assert(vim.uv.fs_lstat(path) == nil)
vim.cmd('edit ' .. vim.fn.fnameescape(path))
local after_open=snapshot()
vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<C-o>',true,false,true),'nx',false)
local after_back=snapshot()
vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<C-i>',true,false,true),'nx',false)
local after_forward=snapshot()
local f=assert(io.open('/private/tmp/twr-nvim-terminal-jump-result.json','w'))
f:write(vim.json.encode({before=before,after_open=after_open,after_back=after_back,after_forward=after_forward})); f:close()
vim.cmd('qa!')
