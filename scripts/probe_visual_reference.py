#!/usr/bin/env python3
"""Reference only: fixed Visual fixtures under clean, private Neovim.

Never loads user configuration, plugins, projects or ShaDa. Results are retained
in an owned temporary directory; no fixture or installed tool is modified.
"""
import json
from pathlib import Path
import subprocess
import tempfile

stage = Path(tempfile.mkdtemp(prefix='tw-nvim-visual-reference-', dir='/private/tmp'))
cases = [
    ('redo-above', 'abcdef\nsecond\nthird', 'lvll<Esc>Onew<Esc>u<C-r>gv'),
    ('undo-line-delete', 'abcdef\nsecond\nthird', 'Vdugv'),
    ('count-goto2', 'uno dos\ntres\ncuatro\n', 'cwnuevo<Esc>2G'),
    ('count-goto3', 'uno dos\ntres\ncuatro\n', 'cwnuevo<Esc>2Gcclinea<Esc>3gg'),
    ('row-insert-before', 'abcdef\nsecond\nthird', 'jlvll<Esc>ggOnew<Esc>gv'),
    ('row-delete-before', 'abcdef\nsecond\nthird', 'jlvll<Esc>ggddgv'),
    ('row-insert-within', 'abcdef\nsecond\nthird', 'lvjll<Esc>ggonew<Esc>gv'),
    ('row-split-before', 'abcdef\nsecond\nthird', 'lvll<Esc>0iX<CR>Y<Esc>gv'),
    ('backward-yank', 'abcdef\nsecond\nthird', '3lvhhy0gv'),
    ('line-char-put', 'abcdef\nsecond\nthird', 'V"apgv'),
    ('repeated-gv', 'abcdef\nsecond\nthird', 'lvll<Esc>gv<Esc>gv'),
    ('forward', 'abcdef\nsecond\nthird', 'lvll<Esc>0gv'),
    ('backward', 'abcdef\nsecond\nthird', '3lvhh<Esc>0gv'),
    ('linewise', 'abcdef\nsecond\nthird', 'Vj<Esc>Ggv'),
    ('swap', 'abcdef\nsecond\nthird', 'lvll<Esc>Gvlgv'),
    ('swap-back', 'abcdef\nsecond\nthird', 'lvll<Esc>Gvlgvgv'),
    ('yank', 'abcdef\nsecond\nthird', 'lvly0gv'),
    ('delete', 'abcdef\nsecond\nthird', 'lvldgv'),
    ('case', 'abcdef\nsecond\nthird', 'lvllUgv'),
    ('replace', 'abcdef\nsecond\nthird', 'lvllrXgv'),
    ('put', 'abcdef\nsecond\nthird', 'lvll"apgv'),
    ('insert-before', 'abcdef\nsecond\nthird', 'lvll<Esc>0iZZ<Esc>gv'),
    ('delete-before', 'abcdef\nsecond\nthird', '3lvl<Esc>0xgv'),
    ('undo-delete', 'abcdef\nsecond\nthird', 'lvldugv'),
]
script = 'local cases = vim.json.decode([=[' + json.dumps(cases) + ']=])\n' + r'''
local results = {}
for _, case in ipairs(cases) do
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Esc>', true, false, true), 'xt', false)
  vim.cmd('enew!')
  local lines = vim.split(case[2], '\n', {plain=true})
  if #lines > 1 and lines[#lines] == '' then table.remove(lines) end
  vim.api.nvim_buf_set_lines(0, 0, -1, false, lines)
  vim.bo.endofline = case[2]:sub(-1) == '\n'
  vim.fn.setreg('a', 'XYZ', 'v')
  vim.api.nvim_win_set_cursor(0, {1,0})
  vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes(case[3], true, false, true), 'xt', false)
  table.insert(results, {label=case[1], source=case[2], sequence=case[3], mode=vim.fn.mode(), cursor=vim.api.nvim_win_get_cursor(0), anchor=vim.fn.getpos('v'), text=vim.api.nvim_buf_get_lines(0,0,-1,false), first=vim.fn.getpos("'<"), last=vim.fn.getpos("'>")})
end
vim.fn.writefile({vim.json.encode(results)}, 'results.json')
vim.cmd('qa!')
'''
(stage / 'probe.lua').write_text(script)
env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage), 'XDG_CONFIG_HOME': str(stage / 'config'), 'XDG_DATA_HOME': str(stage / 'data'), 'XDG_STATE_HOME': str(stage / 'state'), 'XDG_CACHE_HOME': str(stage / 'cache'), 'NVIM_LOG_FILE': str(stage / 'nvim.log')}
version = subprocess.run(['/opt/homebrew/bin/nvim', '--version'], env=env, cwd=stage, capture_output=True, timeout=5, check=True).stdout.decode().splitlines()[0]
(stage / 'metadata.json').write_text(json.dumps({'version': version, 'config': 'NONE', 'shada': 'NONE', 'plugins': False, 'cases': len(cases)}, indent=2) + '\n')
result = subprocess.run(['/opt/homebrew/bin/nvim', '--headless', '-u', 'NONE', '-i', 'NONE', '--noplugin', '-n', '-l', str(stage / 'probe.lua')], env=env, cwd=stage, capture_output=True, timeout=15)
print(stage)
result.check_returncode()
print(version)
print('Cases:', len(json.loads((stage / 'results.json').read_text())))
print('Reference:', stage / 'results.json')
