#!/usr/bin/env python3
"""Bounded, isolated Neovim window focus reference. No user config or task contents."""
import argparse
import random
import json
import subprocess
import tempfile
import time
from pathlib import Path

CASES = [
 ('left-right', [['<C-w>','v'],['<C-w>','h'],['<C-w>','l'],['<C-w>','l'],['<C-w>','h'],['<C-w>','h']]),
 ('top-bottom', [['<C-w>','s'],['<C-w>','k'],['<C-w>','j'],['<C-w>','j'],['<C-w>','k'],['<C-w>','k']]),
 ('right-stack', [['<C-w>','v'],['<C-w>','s'],['<C-w>','t'],['<C-w>','l'],['<C-w>','j'],['<C-w>','k'],['<C-w>','h'],['<C-w>','j']]),
 ('lower-cursor', [['<C-w>','v'],['<C-w>','s'],['<C-w>','t'],['1','5','G'],['<C-w>','l'],['<C-w>','h'],['g','g'],['<C-w>','l']]),
 ('status-boundary', [['<C-w>','v'],['<C-w>','s'],['<C-w>','t'],['1','1','G'],['<C-w>','l'],['<C-w>','h'],['1','3','G'],['<C-w>','l']]),
 ('top-row', [['<C-w>','s'],['<C-w>','t'],['<C-w>','v'],['<C-w>','j'],['<C-w>','k'],['<C-w>','l'],['<C-w>','h']]),
 ('upper-by-column', [['<C-w>','s'],['<C-w>','t'],['<C-w>','v'],['<C-w>','b'],['5','0','l'],['<C-w>','k'],['<C-w>','h']]),
 ('grid', [['<C-w>','v'],['<C-w>','s'],['<C-w>','t'],['<C-w>','s'],['<C-w>','l'],['<C-w>','k'],['<C-w>','h'],['<C-w>','j']]),
 ('five-panel-adjacency', [['<C-w>','v'],['<C-w>','v'],['<C-w>','s'],['<C-w>','t'],['<C-w>','s'],['<C-w>','k'],['<C-w>','j'],['<C-w>','l'],['<C-w>','l'],['<C-w>','j'],['<C-w>','h']]),
 ('nearest-column-gap', [['<C-w>','v'],['<C-w>','v'],['<C-w>','t'],['<C-w>','w'],['<C-w>','s'],['<C-w>','t'],['1','1','G'],['<C-w>','l']]),
]





for seed,label in enumerate(['trace-one','trace-two','trace-three','trace-four',
                             'trace-five','trace-six','trace-seven','trace-eight',
                             'trace-nine','trace-ten','trace-eleven','trace-twelve',
                             'trace-thirteen','trace-fourteen','trace-fifteen','trace-sixteen',
                             'trace-seventeen','trace-eighteen','trace-nineteen','trace-twenty',
                             'trace-twentyone','trace-twentytwo','trace-twentythree','trace-twentyfour',
                             'trace-twentyfive','trace-twentysix','trace-twentyseven','trace-twentyeight'],1):
    randomizer=random.Random(seed)
    commands=[];panes=1
    for index in range(24):
        choices=['h','j','k','l','t','b','w','W','p','position','column']
        if panes<6: choices+=['v','s','v','s']
        key=randomizer.choice(choices)
        if key in ('v','s'): panes+=1
        if key=='position': commands.append(list(str(randomizer.randint(1,100)))+['G'])
        elif key=='column': commands.append(['5','0','l'])
        else: commands.append(['<C-w>',key])
    CASES.append((label,commands))

def rust_fixture(data):
    lines=['// Generated from clean Neovim by probe_editor_pane_focus_reference.py.','const CASES: &[Case] = &[']
    allowed={'<C-w>','v','s','t','b','p','w','W','h','j','k','l','0','1','2','3','4','5','6','7','8','9','G','g'}
    for case in data['cases']:
        assert case['label'].isascii() and all(c.islower() or c=='-' for c in case['label'])
        lines.append('Case { name:'+json.dumps(case['label'])+', steps:&[')
        for keys,item in zip(case['commands'],case['observations']):
            before=item['before'];after=item['after']
            assert set(keys)<=allowed and before['shared_buffer'] and after['shared_buffer']
            assert not after['error']
            assert all(len(row)==5 and all(type(n) is int and 0<=n<=1_000_000_000 for n in row) and row[0]>0 and row[3]>0 and row[4]>0 for row in before['geometry'])
            assert len(before['point'])==2 and all(type(n) is int and abs(n)<=1_000_000_000 for n in before['point'])
            assert all(type(state[key]) is int and 0<=state[key]<=1_000_000_000 for state in [before,after] for key in ['active','previous'])
            assert all(c in 'rowcl0123456789(),' for c in after['layout'])
            bounds=','.join('Bounds { pane: PaneId('+str(row[0])+'), x:'+str(row[1])+', y:'+str(row[2])+', width:'+str(row[3])+', height:'+str(row[4])+' }' for row in before['geometry'])
            point='Point { x:'+str(before['point'][0])+', y:'+str(before['point'][1])+' }'
            lines.append('Step { keys:&['+','.join(json.dumps(k) for k in keys)+'], layout:'+json.dumps(after['layout'])+', before:'+str(before['active'])+', active:'+str(after['active'])+', previous:'+str(after['previous'])+', point:'+point+', bounds:&['+bounds+'] },')
        lines.append('] },')
    lines.append('];');return '\n'.join(lines)+'\n'


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record',action='store_true',help='Intentionally replace the two verified reference fixtures')
    args=parser.parse_args()
    stage = Path(tempfile.mkdtemp(prefix='tw-pane-focus-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    binary = '/opt/homebrew/bin/nvim'
    env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
           'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_DATA_HOME': str(stage/'data'),
           'XDG_STATE_HOME': str(stage/'state'), 'XDG_CACHE_HOME': str(stage/'cache'),
           'NVIM_LOG_FILE': str(stage/'nvim.log')}
    (stage/'setup.lua').write_text('_G.tw_setup=function()\n vim.cmd(\'only!\');vim.cmd(\'enew!\')\n local lines={};for i=1,100 do table.insert(lines,string.rep("a",128)) end;vim.api.nvim_buf_set_lines(0,0,-1,false,lines)\n vim.api.nvim_win_set_cursor(0,{5,2})\n _G.tw_ids={[vim.api.nvim_get_current_win()]=1};_G.tw_next=2\n return true\nend\n_G.tw_observe=function()\n for _,id in ipairs(vim.api.nvim_list_wins()) do\n  if not tw_ids[id] then tw_ids[id]=tw_next;tw_next=tw_next+1 end\n end\n local function tree(node)\n  if node[1]==\'leaf\' then return tostring(tw_ids[node[2]]) end\n  local result={};for _,child in ipairs(node[2]) do table.insert(result,tree(child)) end\n  return node[1]..\'(\'..table.concat(result,\',\')..\')\'\n end\n local cursors={};local buffer=nil;local shared=true\n for _,id in ipairs(vim.api.nvim_list_wins()) do\n  local point=vim.api.nvim_win_get_cursor(id)\n  table.insert(cursors,{tw_ids[id],point[1]-1,point[2]})\n  local current=vim.api.nvim_win_get_buf(id)\n  if buffer and buffer~=current then shared=false end;buffer=current\n end\n table.sort(cursors,function(a,b) return a[1]<b[1] end)\n local geometry={}\n for _,id in ipairs(vim.api.nvim_list_wins()) do\n  local position=vim.api.nvim_win_get_position(id)\n  table.insert(geometry,{tw_ids[id],position[2],position[1],vim.api.nvim_win_get_width(id),vim.api.nvim_win_get_height(id)})\n end\n table.sort(geometry,function(a,b) return a[1]<b[1] end)\n local position=vim.api.nvim_win_get_position(vim.api.nvim_get_current_win())\n local point={position[2]+vim.fn.wincol()-1,position[1]+vim.fn.winline()-1}\n local prior=vim.fn.win_getid(vim.fn.winnr(\'#\'))\n return {layout=tree(vim.fn.winlayout()),active=tw_ids[vim.api.nvim_get_current_win()],previous=tw_ids[prior] or 0,cursors=cursors,shared_buffer=shared,geometry=geometry,point=point,error=vim.v.errmsg}\nend\n')
    socket = stage/'rpc.sock'
    process = subprocess.Popen([binary, '--headless', '-u', 'NONE', '-i', 'NONE',
                                '--noplugin', '-n', '--listen', str(socket),
                                '-c', 'set splitright splitbelow scrolloff=4 sidescrolloff=8 splitkeep=screen nowrap', '-c', 'luafile '+str(stage/'setup.lua')], env=env, cwd=stage,
                               stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                               stderr=(stage/'stderr.log').open('wb'))
    deadline = time.monotonic() + 180

    def remote(*args):
        if time.monotonic() >= deadline:
            raise TimeoutError('Overall reference deadline')
        return subprocess.check_output([binary, '--server', str(socket), *args],
                                       env=env, cwd=stage, stderr=subprocess.PIPE, timeout=3).decode()

    results = []
    try:
        startup = time.monotonic() + 5
        while not socket.exists():
            if time.monotonic() >= startup or process.poll() is not None:
                raise RuntimeError('Clean Neovim did not start: ' + str(stage))
            time.sleep(.01)
        for label, commands in CASES:
            remote('--remote-send', '<Esc>')
            remote('--remote-expr', "luaeval('tw_setup()')")
            observations = []
            for command in commands:
                before=json.loads(remote('--remote-expr', "json_encode(luaeval('tw_observe()'))"))
                remote('--remote-expr', "execute('let v:errmsg = \"\"')")
                remote('--remote-send', ''.join(command))
                after=json.loads(remote('--remote-expr', "json_encode(luaeval('tw_observe()'))"))
                observations.append({'before':before,'after':after})
            results.append({'label': label, 'commands': commands, 'observations': observations})
        version = subprocess.check_output([binary, '--version'], env=env, cwd=stage,
                                          timeout=3).decode().splitlines()[0]
        data = {'version': version, 'config': 'NONE', 'shada': 'NONE', 'plugins': False,
                'options':{'splitright':True,'splitbelow':True,'scrolloff':4,'sidescrolloff':8,'splitkeep':'screen','wrap':False},
                'layout': 'single shared synthetic buffer, splitright=true, splitbelow=true, cursor line5 column3',
                'scope': 'Directional focus from actual Neovim window geometry and visible cursor; headless, no GUI/tmux',
                'cases': results}
        output = stage/'results.json'
        output.write_text(json.dumps(data, ensure_ascii=False, indent=2)+'\n')
        generated=rust_fixture(data)
        (stage/'reference.rs').write_text(generated)
        baseline = Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-editor-pane-focus.json'
        if args.record:
            if baseline.is_symlink() or baseline.with_suffix('.rs').is_symlink():
                raise RuntimeError('Refuse symlink fixture')
            baseline.write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n')
            baseline.with_suffix('.rs').write_text(generated)
        if baseline.exists() and json.loads(baseline.read_text()) != data:
            raise RuntimeError('Reference changed; retained evidence at ' + str(output))
        if baseline.exists() and baseline.with_suffix('.rs').read_text() != generated:
            raise RuntimeError('Compiled fixture differs: '+str(stage/'reference.rs'))
        print(json.dumps({'reference': str(output), 'cases': len(results), 'baseline_compared': baseline.exists() and not args.record, 'recorded':args.record}))
    finally:
        try:
            remote('--remote-send', '<Esc>:qa!<CR>')
        except Exception:
            pass
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


if __name__ == '__main__':
    main()
