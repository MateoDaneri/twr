assert(not vim.uv.fs_lstat('/private/tmp/twr-unnamed-reference-nonexistent.txt'))
vim.o.hidden=true
vim.cmd.edit('/private/tmp/twr-unnamed-reference-nonexistent.txt')
vim.cmd.vnew()
local id=vim.api.nvim_get_current_buf()
vim.api.nvim_buf_set_lines(0,0,-1,false,{'SYNTHETIC_DIRTY'})
vim.cmd('quit!')
local before={exists=vim.fn.bufexists(id),listed=vim.fn.buflisted(id),loaded=vim.fn.bufloaded(id)}
vim.cmd('buffer '..id)
print(vim.json.encode({id=id,before=before,current=vim.api.nvim_get_current_buf(),listed=vim.fn.buflisted(id),loaded=vim.fn.bufloaded(id),name=vim.api.nvim_buf_get_name(id),lines=vim.api.nvim_buf_get_lines(id,0,-1,false)}))
vim.cmd('qa!')
