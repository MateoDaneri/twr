-- Synthetic fixture. Run with a private HOME and Neovim --headless --clean -i NONE -n -l.
local lines={"alpha one","alpha two","","","  ","beta one","","gamma","",".IP term","after",".SH section","tail","\fpage","end"}
local output={version=vim.version().major..'.'..vim.version().minor..'.'..vim.version().patch,paragraphs=vim.o.paragraphs,sections=vim.o.sections,cases={}}
for _,row in ipairs({1,2,3,4,5,6,7,8,9,10,11,12,13,14,15}) do
 for _,keys in ipairs({'}','{','2}','2{','9}','9{'}) do
  vim.api.nvim_buf_set_lines(0,0,-1,false,lines)
  vim.api.nvim_win_set_cursor(0,{row,math.min(2,#lines[row])})
  local start=vim.api.nvim_win_get_cursor(0)
  vim.cmd('normal! '..keys)
  table.insert(output.cases,{row=row,key=keys,start=start,finish=vim.api.nvim_win_get_cursor(0)})
 end
end
print(vim.json.encode(output))