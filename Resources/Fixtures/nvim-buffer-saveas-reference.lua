-- Run with nvim --headless -n --clean -l this-file and TWR_RESEARCH_ROOT
-- pointing to a fresh /private/tmp/twr-saveas-[a|b]-* directory.
-- This fixture writes one synthetic file inside that directory.
local root=assert(vim.env.TWR_RESEARCH_ROOT)
assert(root:match('^/private/tmp/twr%-saveas%-[ab]%-[%w]+$'))
assert(vim.uv.fs_realpath(root) == root and vim.uv.fs_stat(root).type == 'directory')
assert(not vim.uv.fs_lstat(root..'/new.txt'), 'refusing to overwrite a file')
vim.o.hidden=true
vim.cmd.edit(root..'/old.txt')
local before=vim.api.nvim_get_current_buf()
vim.api.nvim_buf_set_lines(0,0,-1,false,{'SYNTHETIC_CONTENT'})
vim.cmd.saveas(root..'/new.txt')
local current=vim.api.nvim_get_current_buf()
local old=vim.fn.bufnr(root..'/old.txt')
local result={before=before,after=current,old=old,old_exists=vim.fn.bufexists(old),old_listed=vim.fn.buflisted(old),old_loaded=vim.fn.bufloaded(old),alternate=vim.fn.bufnr('#'),new=vim.fn.bufnr(root..'/new.txt')}
print(vim.json.encode(result))
vim.cmd('qa!')
