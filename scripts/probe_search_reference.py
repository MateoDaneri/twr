#!/usr/bin/env python3
"""Repeat only fixed, synthetic pattern cases in clean Neovim; retain results."""
import json
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
fixture = json.loads((root / 'Resources/Fixtures/nvim-pattern-reference.json').read_text())
cases = [[c['label'], c['text'], c['query']] for c in fixture['cases']]
stage = Path(tempfile.mkdtemp(prefix='tw-search-reference-', dir='/private/tmp'))
print(stage, flush=True)
script = 'local cases=vim.json.decode([=[' + json.dumps(cases) + ']=])\n' + r'''
vim.o.ignorecase=true; vim.o.smartcase=true
local out={}
for _,c in ipairs(cases) do
 vim.cmd('enew!')
 local lines=vim.split(c[2],'\n',{plain=true})
 if #lines>1 and lines[#lines]=='' then table.remove(lines) end
 vim.api.nvim_buf_set_lines(0,0,-1,false,lines)
 vim.bo.endofline=c[2]:sub(-1)=='\n'
 vim.api.nvim_win_set_cursor(0,{1,0})
 local positions={}; local err=nil
 for n=1,100001 do
  local ok,pos=pcall(vim.fn.searchpos,c[3],n==1 and 'Wc' or 'W')
  if not ok then err=pos; break end
  if pos[1]==0 then break end
  local cursor=vim.api.nvim_win_get_cursor(0)
  local offset=vim.str_utfindex(lines[cursor[1]],'utf-32',cursor[2],false)
  for row=1,cursor[1]-1 do offset=offset+vim.str_utfindex(lines[row])+1 end
  if positions[#positions]~=offset then positions[#positions+1]=offset end
 end
 out[#out+1]={label=c[1],text=c[2],query=c[3],expected_positions=positions,error=err}
end
vim.fn.writefile({vim.json.encode(out)},'results.json');vim.cmd('qa!')
'''
(stage / 'probe.lua').write_text(script)
env = {'HOME':str(stage), 'PATH':'/usr/bin:/bin', 'TMPDIR':str(stage), 'XDG_CONFIG_HOME':str(stage/'config'), 'XDG_DATA_HOME':str(stage/'data'), 'XDG_STATE_HOME':str(stage/'state'), 'XDG_CACHE_HOME':str(stage/'cache'), 'NVIM_LOG_FILE':str(stage/'nvim.log')}
version = subprocess.run(['/opt/homebrew/bin/nvim','--version'],env=env,cwd=stage,capture_output=True,timeout=5,check=True).stdout.decode().splitlines()[0]
(stage/'metadata.json').write_text(json.dumps({'version':version,'config':'NONE','shada':'NONE','plugins':False,'ignorecase':True,'smartcase':True,'cases':len(cases)},indent=2)+'\n')
subprocess.run(['/opt/homebrew/bin/nvim','--headless','-u','NONE','-i','NONE','--noplugin','-n','-l',str(stage/'probe.lua')],env=env,cwd=stage,capture_output=True,timeout=15,check=True)
actual=json.loads((stage/'results.json').read_text())
if len(actual) != len(fixture['cases']):
    raise RuntimeError('Reference case count changed: '+str(stage))
for expected, observed in zip(fixture['cases'],actual):
    if observed.get('error') or expected['expected_positions']!=observed['expected_positions']:
        raise RuntimeError('Reference changed: '+observed['label']+'; results retained at '+str(stage))
print(version)
print('Cases:',len(actual))
print('Reference:',stage/'results.json')
