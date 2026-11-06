#!/usr/bin/env python3
"""Check window-local Neovim scroll options in an isolated synthetic session."""
import json
import subprocess
import tempfile
from pathlib import Path

stage = Path(tempfile.mkdtemp(prefix='twr-scroll-windows-', dir='/private/tmp'))
stage.chmod(0o700)
env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
       'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_STATE_HOME': str(stage/'state'),
       'XDG_DATA_HOME': str(stage/'data'), 'XDG_CACHE_HOME': str(stage/'cache'),
       'NVIM_LOG_FILE': str(stage/'nvim.log')}
lua = r'''
vim.o.lines=30
vim.cmd.split()
vim.cmd('resize 10')
vim.wo.scroll=3
local first=vim.api.nvim_get_current_win()
vim.cmd.vsplit()
local second=vim.api.nvim_get_current_win()
local out={}
local function save(name)
  out[name]={vim.api.nvim_win_get_height(first),vim.wo[first].scroll,
             vim.api.nvim_win_get_height(second),vim.wo[second].scroll}
end
save('split')
vim.wo[second].scroll=6
save('separate')
vim.cmd('resize 8')
save('resize')
vim.wo[second].scroll=2
vim.api.nvim_set_current_win(first)
vim.cmd.enew()
vim.api.nvim_set_current_win(second)
save('switch_and_buffer')
print(vim.json.encode(out))
'''
result = subprocess.run(['/opt/homebrew/bin/nvim', '--headless', '-u', 'NONE', '-i', 'NONE',
                         '--noplugin', '-n', '-c', 'lua ' + lua.replace('\n', ' '), '-c', 'qa!'],
                        cwd=stage, env=env, text=True, capture_output=True, timeout=15, check=True)
actual = json.loads(result.stderr.strip())
expected = {'split': [10, 3, 10, 5], 'separate': [10, 3, 10, 6],
            'resize': [8, 4, 8, 4], 'switch_and_buffer': [8, 4, 8, 2]}
if actual != expected:
    raise RuntimeError('Window-local reference changed: ' + json.dumps(actual))
print(json.dumps(actual, sort_keys=True))
