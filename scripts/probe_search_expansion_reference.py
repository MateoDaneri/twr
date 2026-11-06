#!/usr/bin/env python3
"""Fixed incremental-search expansion cases over clean headless Neovim RPC.
No user config, histories, projects, UI automation or network.
"""
import json
from pathlib import Path
import subprocess
import tempfile
import time

CASES = []
for mode in ('', r'\v', r'\M', r'\V'):
    for char in '.*[]^$\\/?+':
        CASES.append({'label': 'char-' + repr(char) + '-mode-' + (mode or 'default'),
                      'text': 'start foo' + char + 'bar foo' + char + 'baz',
                      'query': mode + 'foo', 'keys': ['<C-l>']})
CASES += [
    {'label':'word-tail','text':'start foobar foobaz','query':'fo','keys':['<C-r>','<C-w>']},
    {'label':'complete-word','text':'start foo.bar baz','query':'foo','keys':['<C-r>','<C-w>']},
    {'label':'punctuation-word','text':'start foo.bar baz','query':'foo','keys':['<C-l>','<C-r>','<C-w>']},
    {'label':'lowercase-extension','text':'start foOBAR','query':'fo','keys':['<C-l>']},
    {'label':'uppercase-extension','text':'start FOOBAR','query':'FO','keys':['<C-l>']},
    {'label':'word-uppercase-tail','text':'start FOOBAR','query':'fo','keys':['<C-r>','<C-w>']},
    {'label':'unicode','text':'start ñÁfono','query':'ñ','keys':['<C-l>','<C-r>','<C-w>']},
    {'label':'no-match','text':'start foobar','query':'absent','keys':['<C-l>']},
    {'label':'line-end','text':'start foo\nbar','query':'foo','keys':['<C-l>']},
    {'label':'empty-word','text':'foobar','query':'','keys':['<C-r>','<C-w>']},
    {'label':'next-match','text':'start fooOne fooTwo','query':'foo','keys':['<C-g>','<C-l>']},
    {'label':'reverse','text':'start fooOne fooTwo','query':'foo','prefix':'?','keys':['<C-l>']},
]


def main(cases=CASES, baseline_name="nvim-search-expansion.json"):
    stage = Path(tempfile.mkdtemp(prefix='tw-search-expansion-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    binary = '/opt/homebrew/bin/nvim'
    env = {'HOME':str(stage), 'PATH':'/usr/bin:/bin', 'TMPDIR':str(stage),
           'XDG_CONFIG_HOME':str(stage/'config'), 'XDG_STATE_HOME':str(stage/'state'),
           'XDG_DATA_HOME':str(stage/'data'), 'XDG_CACHE_HOME':str(stage/'cache'),
           'NVIM_LOG_FILE':str(stage/'nvim.log')}
    lua = 'local cases=vim.json.decode([=[' + json.dumps(cases) + ']=])\n' + r'''
_G.tw_case=function(index)
 local case=cases[index]
 vim.cmd('enew!');vim.fn.setreg('/','')
 vim.api.nvim_buf_set_lines(0,0,-1,false,vim.split(case.text,'\n',{plain=true}))
 vim.api.nvim_win_set_cursor(0,{1,0})
 return true
end
'''
    (stage/'setup.lua').write_text(lua)
    socket = stage/'rpc.sock'
    process = subprocess.Popen([binary,'--headless','-u','NONE','-i','NONE','--noplugin','-n',
                                '--listen',str(socket),'-c','set ignorecase smartcase incsearch',
                                '-c','luafile '+str(stage/'setup.lua')],
                               env=env,cwd=stage,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    def remote(*args):
        return subprocess.check_output([binary,'--server',str(socket),*args],env=env,cwd=stage,
                                       stderr=subprocess.PIPE,timeout=4).decode()
    def observe():
        return json.loads(remote('--remote-expr', "json_encode({'mode':mode(),'command':getcmdline()})"))
    results = []
    try:
        deadline = time.monotonic()+5
        while not socket.exists():
            if time.monotonic()>deadline or process.poll() is not None:
                raise RuntimeError('Clean Neovim RPC did not start; evidence at '+str(stage))
            time.sleep(.01)
        for index,case in enumerate(cases,1):
            remote('--remote-send','<Esc>')
            remote('--remote-expr',"luaeval('tw_case("+str(index)+")')")
            remote('--remote-send',case.get('prefix','/')+case['query'])
            time.sleep(.03)
            history=[]
            for key in case['keys']:
                remote('--remote-send',key)
                time.sleep(.03)
                if key != '<C-r>':
                    history.append(observe())
            results.append({**case,'observations':history,'expected_command':history[-1]['command'],'expected_mode':history[-1]['mode']})
            (stage/'partial.json').write_text(json.dumps(results,ensure_ascii=False,indent=2)+'\n')
        version = subprocess.check_output([binary,'--version'],env=env,cwd=stage,timeout=5).decode().splitlines()[0]
        output = {'version':version,'config':'NONE','shada':'NONE','plugins':False,
                  'ignorecase':True,'smartcase':True,'incsearch':True,'cases':results}
        (stage/'results.json').write_text(json.dumps(output,ensure_ascii=False,indent=2)+'\n')
        if baseline_name is not None:
            expected = json.loads((Path(__file__).resolve().parents[1] / 'Resources/Fixtures' / baseline_name).read_text())
            def signature(case):
                return (case['label'], case['expected_command'], case.get('expected_mode',case['observations'][-1]['mode']))
            if [signature(case) for case in results] != [signature(case) for case in expected['cases']]:
                raise RuntimeError('Neovim reference changed; inspect retained evidence at ' + str(stage))
        print(json.dumps({'reference':str(stage/'results.json'),'version':version,'cases':len(results),'baseline_compared':baseline_name is not None}),flush=True)
    finally:
        try: remote('--remote-send','<Esc>:qa!<CR>')
        except Exception: pass
        try: process.wait(timeout=3)
        except subprocess.TimeoutExpired: process.kill();process.wait()


if __name__=='__main__':
    main()
