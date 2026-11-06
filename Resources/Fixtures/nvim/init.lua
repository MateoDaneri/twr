vim.opt.termguicolors = true
vim.opt.number = true
vim.opt.mouse = "a"
vim.opt.swapfile = false
vim.opt.shadafile = "NONE"
vim.opt.laststatus = 2
vim.cmd("syntax on")
for key, direction in pairs({h = "h", j = "j", k = "k", l = "l"}) do
  vim.keymap.set("n", "<C-" .. key .. ">", "<C-w>" .. direction)
end
