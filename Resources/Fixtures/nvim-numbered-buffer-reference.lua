-- Run with: nvim --headless -n --clean -l this-file
-- Uses synthetic, nonexistent paths and never writes project files. IDs in
-- this fixture are from a fresh Neovim 0.12.1 process, not the user's session.
vim.o.hidden = true
local root = '/private/tmp/twr-buffer-id-reference/'
local out = {}
local function current() return vim.api.nvim_get_current_buf() end
local function info(buf)
  return {id=buf, exists=vim.fn.bufexists(buf), listed=vim.fn.buflisted(buf), loaded=vim.fn.bufloaded(buf)}
end
local function key(s)
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes(s, true, false, true), 'nx', false)
end
vim.cmd.edit(root..'one.txt')
local one=current()
vim.cmd.edit(root..'two.txt')
local two=current()
vim.cmd.edit(root..'three.txt')
local three=current()
out.created={one=one,two=two,three=three,current=current(), alternate=vim.fn.bufnr('#')}
key(two..'<C-^>'); out.numbered_switch={current=current(),alternate=vim.fn.bufnr('#')}
vim.cmd('buffer '..three)
vim.cmd('bdelete '..two)
out.deleted=info(two)
local readd=vim.fn.bufadd(root..'two.txt')
out.readd={id=readd, state=info(readd)}
vim.cmd('buffer '..readd)
out.reopened={current=current(),state=info(readd)}
vim.cmd.file(root..'renamed.txt')
out.renamed={current=current(), old=vim.fn.bufnr(root..'two.txt'), new=vim.fn.bufnr(root..'renamed.txt')}
vim.cmd('buffer '..three)
vim.cmd('bwipeout '..readd)
out.wiped=info(readd)
local again=vim.fn.bufadd(root..'renamed.txt')
out.after_wipe={id=again,state=info(again)}
print(vim.json.encode(out))
vim.cmd('qa!')
