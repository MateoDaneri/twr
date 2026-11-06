-- Synthetic reference; no file is written and the named path must be absent.
assert(not vim.uv.fs_lstat('/private/tmp/twr-unnamed-reference-nonexistent.txt'))
vim.o.hidden=true
vim.cmd.edit('/private/tmp/twr-unnamed-reference-nonexistent.txt')
vim.cmd.enew()
local id=vim.api.nvim_get_current_buf()
vim.api.nvim_buf_set_lines(0,0,-1,false,{'TEST_DIRTY'})
vim.cmd('bdelete! '..id)
print(vim.json.encode({id=id,exists=vim.fn.bufexists(id),listed=vim.fn.buflisted(id),loaded=vim.fn.bufloaded(id),current=vim.api.nvim_get_current_buf()}))
vim.cmd('qa!')
