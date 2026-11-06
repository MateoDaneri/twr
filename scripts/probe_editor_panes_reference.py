#!/usr/bin/env python3
"""Bounded, isolated Neovim window focus reference. No user config or task contents."""
import json
import subprocess
import tempfile
import time
from pathlib import Path

CASES = [
    ('vertical-horizontal-close', [['<C-w>', 'v'], ['<C-w>', 's'], ['<C-w>', 'c'], ['<C-w>', 'c'], ['<C-w>', 'c']]),
    ('row-flatten', [['<C-w>', 'v'], ['<C-w>', 'v'], ['<C-w>', 'v'], ['<C-w>', 'c'], ['<C-w>', 't'], ['<C-w>', 'c']]),
    ('mixed-first', [['<C-w>', 'v'], ['<C-w>', 's'], ['<C-w>', 'v'], ['<C-w>', 't'], ['<C-w>', 'v']]),
    ('close-first', [['<C-w>', 'v'], ['<C-w>', 's'], ['<C-w>', 't'], ['<C-w>', 'c'], ['<C-w>', 'p']]),
    ('close-last', [['<C-w>', 'v'], ['<C-w>', 's'], ['<C-w>', 'b'], ['<C-w>', 'c'], ['<C-w>', 'p']]),
    ('cycle-history', [['<C-w>', 'v'], ['<C-w>', 's'], ['<C-w>', 't'], ['<C-w>', 'w'], ['<C-w>', 'W'], ['<C-w>', 'p']]),
    ('close-middle', [['<C-w>', 'v'], ['<C-w>', 's'], ['<C-w>', 't'], ['<C-w>', 'w'], ['<C-w>', 'c']]),
    ('no-previous', [['<C-w>', 'p']]),
    ('independent-cursors', [['<C-w>', 'v'], ['j'], ['<C-w>', 'p'], ['<C-w>', 'p'], ['<C-w>', 's']]),
]





def rust_fixture(data):
    lines=['// Generated from clean Neovim observations by probe_editor_panes_reference.py.', 'const CASES: &[Case] = &[']
    allowed={'<C-w>','v','s','c','t','b','p','w','W','j'}
    for case in data['cases']:
        assert case['label'].isascii() and all(c.islower() or c=='-' for c in case['label'])
        lines.append('Case { name:'+json.dumps(case['label'])+', steps:&[')
        for keys,state in zip(case['commands'],case['observations']):
            assert set(keys) <= allowed and state['shared_buffer']
            assert all(c in 'rowcl0123456789(),' for c in state['layout'])
            assert all(type(n) is int and n>=0 for item in state['cursors'] for n in item)
            assert type(state['active']) is int and type(state['previous']) is int
            cursors=','.join('('+','.join(str(n) for n in item)+')' for item in state['cursors'])
            lines.append('Step { keys:&['+','.join(json.dumps(k) for k in keys)+'], layout:'+json.dumps(state['layout'])+', active:'+str(state['active'])+', previous:'+str(state['previous'])+', cursors:&['+cursors+'], error:'+str(bool(state['error'])).lower()+' },')
        lines.append('] },')
    lines.append('];')
    return '\n'.join(lines)+'\n'


def main(compare_baseline=True):
    stage = Path(tempfile.mkdtemp(prefix='tw-editor-panes-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    binary = '/opt/homebrew/bin/nvim'
    env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
           'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_DATA_HOME': str(stage/'data'),
           'XDG_STATE_HOME': str(stage/'state'), 'XDG_CACHE_HOME': str(stage/'cache'),
           'NVIM_LOG_FILE': str(stage/'nvim.log')}
    (stage/'setup.lua').write_text("_G.tw_setup=function()\n vim.cmd('only!');vim.cmd('enew!')\n vim.api.nvim_buf_set_lines(0,0,-1,false,{'alpha','alpha','alpha','alpha','alpha','alpha','alpha','alpha','alpha','alpha'})\n vim.api.nvim_win_set_cursor(0,{5,2})\n _G.tw_ids={[vim.api.nvim_get_current_win()]=1};_G.tw_next=2\n return true\nend\n_G.tw_observe=function()\n for _,id in ipairs(vim.api.nvim_list_wins()) do\n  if not tw_ids[id] then tw_ids[id]=tw_next;tw_next=tw_next+1 end\n end\n local function tree(node)\n  if node[1]=='leaf' then return tostring(tw_ids[node[2]]) end\n  local result={};for _,child in ipairs(node[2]) do table.insert(result,tree(child)) end\n  return node[1]..'('..table.concat(result,',')..')'\n end\n local cursors={};local buffer=nil;local shared=true\n for _,id in ipairs(vim.api.nvim_list_wins()) do\n  local point=vim.api.nvim_win_get_cursor(id)\n  table.insert(cursors,{tw_ids[id],point[1]-1,point[2]})\n  local current=vim.api.nvim_win_get_buf(id)\n  if buffer and buffer~=current then shared=false end;buffer=current\n end\n table.sort(cursors,function(a,b) return a[1]<b[1] end)\n local prior=vim.fn.win_getid(vim.fn.winnr('#'))\n return {layout=tree(vim.fn.winlayout()),active=tw_ids[vim.api.nvim_get_current_win()],previous=tw_ids[prior] or 0,cursors=cursors,shared_buffer=shared,error=vim.v.errmsg}\nend\n")
    socket = stage/'rpc.sock'
    process = subprocess.Popen([binary, '--headless', '-u', 'NONE', '-i', 'NONE',
                                '--noplugin', '-n', '--listen', str(socket),
                                '-c', 'set splitright splitbelow', '-c', 'luafile '+str(stage/'setup.lua')], env=env, cwd=stage,
                               stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                               stderr=(stage/'stderr.log').open('wb'))
    deadline = time.monotonic() + 45

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
                remote('--remote-expr', "execute('let v:errmsg = \"\"')")
                remote('--remote-send', ''.join(command))
                observations.append(json.loads(remote('--remote-expr',
                    "json_encode(luaeval('tw_observe()'))")))
            results.append({'label': label, 'commands': commands, 'observations': observations})
        version = subprocess.check_output([binary, '--version'], env=env, cwd=stage,
                                          timeout=3).decode().splitlines()[0]
        data = {'version': version, 'config': 'NONE', 'shada': 'NONE', 'plugins': False,
                'layout': 'single shared synthetic buffer, splitright=true, splitbelow=true, cursor line5 column3',
                'scope': 'Editor pane topology, focus history and per-window cursors only; no geometry/GUI/tmux',
                'cases': results}
        output = stage/'results.json'
        output.write_text(json.dumps(data, ensure_ascii=False, indent=2)+'\n')
        generated=rust_fixture(data)
        (stage/'reference.rs').write_text(generated)
        baseline = Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-editor-panes.json'
        if compare_baseline and baseline.exists() and json.loads(baseline.read_text()) != data:
            raise RuntimeError('Reference changed; retained evidence at ' + str(output))
        if compare_baseline and baseline.exists() and baseline.with_suffix('.rs').read_text() != generated:
            raise RuntimeError('Compiled fixture differs: '+str(stage/'reference.rs'))
        print(json.dumps({'reference': str(output), 'cases': len(results), 'baseline_compared': compare_baseline and baseline.exists()}))
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
