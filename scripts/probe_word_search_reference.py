#!/usr/bin/env python3
"""Fixed * / # / g* / g# cases; clean Neovim only, no personal state."""
import json
from pathlib import Path
import subprocess
import tempfile

stage=Path(tempfile.mkdtemp(prefix='tw-word-search-reference-',dir='/private/tmp'))
cases=[
 ('star','Foo foo foobar FOO','*'), ('hash','Foo foo foobar FOO','#'),
 ('count','Foo foo foobar FOO','2*'), ('inside-reverse','Foo foo foobar FOO','l#'),
 ('substring','Foo foo foobar FOO','g*g*'), ('substring-count','Foo foo foobar FOO','2g*'),
 ('substring-reverse','Foo foo foobar FOO','g#'), ('repeat-case','Foo foo foobar FOO','*n'),
 ('keyword-after','++ foo ++ foo','*'), ('blank-punctuation','  ++ ++','*'),
 ('punctuation','++ ++','*'), ('punctuation-tail','foo!! foo!!','3l*'),
 ('unicode','ñ ññ ñ','*'), ('underscore','foo_bar foo foobar foo_bar','*'),
 ('visual-forward','Foo foo foobar FOO','vll*'), ('visual-reverse','Foo foo foobar FOO','vll#'),
 ('visual-lower','foo foobar foo','vll*'), ('visual-reverse-count','foo foo foo','vll#'),
 ('literal-backslash','a\\c a\\c','vll*'), ('no-word','   ','*'),
]
script='local cases=vim.json.decode([=['+json.dumps(cases)+']=])\n'+r'''
vim.o.ignorecase=true;vim.o.smartcase=true
local out={}
for _,c in ipairs(cases) do
 vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes('<Esc>',true,false,true),'xt',false)
 vim.cmd('enew!'); vim.fn.setreg('/','')
 local lines=vim.split(c[2],'\n',{plain=true})
 if #lines>1 and lines[#lines]=='' then table.remove(lines) end
 vim.api.nvim_buf_set_lines(0,0,-1,false,lines)
 vim.bo.endofline=c[2]:sub(-1)=='\n'
 vim.api.nvim_win_set_cursor(0,{1,0})
 vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes(c[3],true,false,true),'xt',false)
 local cur=vim.api.nvim_win_get_cursor(0)
 out[#out+1]={label=c[1],source=c[2],sequence=c[3],cursor=cur,character=vim.str_utfindex(vim.api.nvim_get_current_line(),'utf-32',cur[2],false),pattern=vim.fn.getreg('/'),direction=vim.v.searchforward,mode=vim.fn.mode()}
end
vim.fn.writefile({vim.json.encode(out)},'results.json');vim.cmd('qa!')
'''
(stage/'probe.lua').write_text(script)
env={'HOME':str(stage),'PATH':'/usr/bin:/bin','TMPDIR':str(stage),'XDG_CONFIG_HOME':str(stage/'config'),'XDG_STATE_HOME':str(stage/'state'),'XDG_DATA_HOME':str(stage/'data'),'XDG_CACHE_HOME':str(stage/'cache'),'NVIM_LOG_FILE':str(stage/'nvim.log')}
version=subprocess.run(['/opt/homebrew/bin/nvim','--version'],cwd=stage,env=env,capture_output=True,timeout=5,check=True).stdout.decode().splitlines()[0]
(stage/'metadata.json').write_text(json.dumps({'version':version,'config':'NONE','shada':'NONE','plugins':False,'ignorecase':True,'smartcase':True,'cases':len(cases)},indent=2)+'\n')
subprocess.run(['/opt/homebrew/bin/nvim','--headless','-u','NONE','-i','NONE','--noplugin','-n','-l',str(stage/'probe.lua')],cwd=stage,env=env,capture_output=True,timeout=15,check=True)
print(version)
for case in json.loads((stage/'results.json').read_text()): print(case['label'],case['character'],case['mode'])
print('Reference:',stage/'results.json')
