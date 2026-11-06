-- Run with nvim --headless -n --clean -l this-file. These names must not
-- exist; the fixture does not write either path.
assert(not vim.uv.fs_lstat('/private/tmp/twr-file-rename-demo-old.txt'))
assert(not vim.uv.fs_lstat('/private/tmp/twr-file-rename-demo-new.txt'))
vim.o.hidden=true
vim.cmd.edit('/private/tmp/twr-file-rename-demo-old.txt')
local id=vim.api.nvim_get_current_buf()
vim.cmd.file('/private/tmp/twr-file-rename-demo-new.txt')
local old=vim.fn.bufnr('/private/tmp/twr-file-rename-demo-old.txt')
print(vim.json.encode({current=vim.api.nvim_get_current_buf(),original=id,old=old,old_listed=vim.fn.buflisted(old),old_loaded=vim.fn.bufloaded(old),alternate=vim.fn.bufnr('#')}))
vim.cmd('qa!')
