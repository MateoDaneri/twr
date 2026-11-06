-- Synthetic buffers only. Run nvim --headless -u NONE -i NONE -n -l this-file.
local results = {}
local function run(name, text, macro, command)
  vim.cmd.enew({ bang = true })
  vim.api.nvim_buf_set_lines(0, 0, -1, false, { text })
  vim.fn.setreg("a", macro, "v")
  vim.cmd.normal({ args = { command }, bang = true })
  results[name] = { text = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), "\n"), cursor = vim.api.nvim_win_get_cursor(0) }
end
run("counted", "abc", "x", "2@a")
vim.cmd.normal({ args = { "@@" }, bang = true })
results.last = { text = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), "\n") }
run("unicode", "z", "iñ界" .. string.char(27), "@a")
run("literal_notation", "z", "A<Esc>" .. string.char(27), "@a")
vim.cmd.enew({ bang = true })
vim.api.nvim_buf_set_lines(0, 0, -1, false, { "z" })
vim.fn.setreg("a", "iX" .. string.char(27), "v")
local keys = vim.api.nvim_replace_termcodes("qAA!<Esc>q", true, false, true)
vim.api.nvim_feedkeys(keys, "xt", false)
results.append_text = { register = vim.fn.getreg("a") }
vim.api.nvim_buf_set_lines(0, 0, -1, false, { "z" })
vim.api.nvim_win_set_cursor(0, { 1, 0 })
vim.cmd.normal({ args = { "@a" }, bang = true })
results.append_text.text = vim.api.nvim_get_current_line()
print(vim.json.encode(results))
vim.cmd("qa!")
