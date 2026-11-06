local original = { 'top', '  alpha', '    deepA', '    deepB', '  omega', '', 'tail', '  secondA', '  secondB', 'end' }
local scenarios = {{'zM','j','zo','>>','j','.'}, {'zM','j','zo','j','>>','j','.'}, {'zM','j','zo','2>>','j','.'}, {'zM','j','zo','<<','j','.'}, {'zM','j','zo','j','<<','j','.'}, {'zM','j','zo','2<<','j','.'}, {'zM','j','zo','j','zo','>>','j','.'}, {'zM','j','zo','j','zo','2>>','j','.'}, {'zM','j','>>','j','.'}, {'zM','j','zo','>>','u'}, {'zM','j','zo','j','>>','u'}, {'zM','j','zo','j','j','>>','j','.'}, {'zM','j','zo','j','j','<<','j','.'}, {'zM','j','zo','j','zo','j','>>','j','.'}, {'zM','j','zo','j','zo','j','<<','j','.'}, {'zM','j','zo','j','zo','j','j','>>'}, {'zM','j','zo','j','zo','j','j','<<'}, {'zM','j','zo','j','zo','>>','u'}, {'2j','zc','k','>>','j','.'}, {'2j','zc','k','<<','j','.'}, {'2j','zc','j','>>','k','.'}, {'2j','zc','j','<<','k','.'}, {'zM','j','zo','j','zo','j','j','j','>>'}, {'zM','j','zo','j','zo','j','j','j','<<'}, {'zM','j','zo','j','zo','j','j','j','j','>>'}, {'zM','j','zo','3>>','j','.'}, {'zM','j','zo','3<<','j','.'}, {'2j','zc','k','2>>','j','.'}, {'2j','zc','k','2<<','j','.'}, {'2j','zc','k','3>>','j','.'}, {'2j','zc','k','3<<','j','.'}, {'zM','j','zo','j','2>>','j','.'}, {'zM','j','zo','j','2<<','j','.'}}
local out = {}
for _, steps in ipairs(scenarios) do
  vim.cmd('enew!')
  vim.bo.undolevels = -1
  vim.api.nvim_buf_set_lines(0, 0, -1, false, original)
  vim.bo.undolevels = 1000
  vim.opt_local.foldmethod='indent'
  vim.opt_local.foldlevel=99
  vim.opt_local.shiftwidth=2
  vim.opt_local.tabstop=2
  vim.opt_local.expandtab=true
  vim.opt_local.foldminlines=1
  local states={}
  for _, keys in ipairs(steps) do
    vim.cmd('normal! ' .. keys)
    vim.cmd('redraw')
    local folds={}
    for i=1,#original do folds[i] = {vim.fn.foldlevel(i),vim.fn.foldclosed(i),vim.fn.foldclosedend(i)} end
    states[#states+1] = {keys=keys, line=vim.fn.line('.'), text=vim.api.nvim_buf_get_lines(0,0,-1,false), folds=folds}
  end
  out[#out+1]=states
end
io.stdout:write(vim.json.encode(out) .. '\n')
vim.cmd('qa!')
