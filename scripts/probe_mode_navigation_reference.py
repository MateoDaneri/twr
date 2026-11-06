#!/usr/bin/env python3
"""Verify mode transitions against clean Neovim; synthetic HOME/text, no plugins."""
import json
from pathlib import Path
import subprocess
import tempfile
import time


def main():
    stage = Path(tempfile.mkdtemp(prefix='tw-mode-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    env = {'HOME': str(stage), 'TMPDIR': str(stage), 'PATH': '/usr/bin:/bin',
           'NVIM_LOG_FILE': str(stage / 'nvim.log'),
           **{f'XDG_{name}_HOME': str(stage / name.lower()) for name in ('CONFIG', 'STATE', 'DATA', 'CACHE')}}
    binary = '/opt/homebrew/bin/nvim'
    socket = stage / 'rpc.sock'
    process = subprocess.Popen([binary, '--headless', '-u', 'NONE', '-i', 'NONE',
                                '--noplugin', '-n', '--listen', str(socket)],
                               cwd=stage, env=env, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    def remote(*args):
        return subprocess.check_output([binary, '--server', str(socket), *args],
                                       cwd=stage, env=env, stderr=subprocess.PIPE, timeout=4).decode()
    observations = []
    try:
        deadline = time.monotonic() + 5
        while not socket.exists():
            if process.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError('Clean Neovim RPC unavailable: ' + str(stage))
            time.sleep(.01)
        remote('--remote-expr', "luaeval('vim.api.nvim_buf_set_lines(0,0,-1,false,{\"abc\",\"next\"})')")
        for keys, expected_mode, expected_lines in [
            ('iX', 'i', ['Xabc', 'next']),
            ('<C-h>', 'i', ['abc', 'next']),
            ('<C-j>', 'i', ['', 'abc', 'next']),
            ('<Esc>', 'n', ['', 'abc', 'next']),
            ('v', 'v', None), ('<Esc>', 'n', None),
            ('V', 'V', None), ('<Esc>', 'n', None),
            ('<C-v>', '\x16', None), ('<Esc>', 'n', None),
            (':', 'c', None), ('<Esc>', 'n', None),
            ('/', 'c', None), ('<Esc>', 'n', None),
            ('?', 'c', None), ('<Esc>', 'n', None),
            (':enew<C-j>', 'n', ['']),
        ]:
            # Hidden dirty buffer is intentional, matching the workspace's retained buffers.
            if keys == ':enew<C-j>':
                remote('--remote-expr', "execute('set hidden')")
            remote('--remote-send', keys)
            time.sleep(.03)
            observed = json.loads(remote('--remote-expr', "json_encode({'mode':mode(),'lines':getline(1,'$')})"))
            observations.append({'keys': keys, **observed})
            assert observed['mode'] == expected_mode, observations[-1]
            if expected_lines is not None:
                assert observed['lines'] == expected_lines, observations[-1]
        window_cases = []
        for right in (False, True):
            remote('--remote-expr', "execute('silent! %bwipeout! | enew | file content')")
            remote('--remote-expr', "execute('%s vnew | file tree | belowright new | file agents')" % ('botright' if right else 'topleft'))
            for start, keys in [('content','<C-w>h'), ('content','<C-w>l'),
                                ('tree','<C-w>j'), ('agents','<C-w><C-k>'),
                                ('tree','<C-w>w'), ('agents','<C-w>w'),
                                ('content','<C-w>w'), ('content','<C-w>W')]:
                remote('--remote-expr', "luaeval('(function() for _,w in ipairs(vim.api.nvim_tabpage_list_wins(0)) do if vim.fn.bufname(vim.api.nvim_win_get_buf(w)) == \"%s\" then vim.api.nvim_set_current_win(w) end end return true end)()')" % start)
                remote('--remote-send', keys)
                time.sleep(.03)
                target = remote('--remote-expr', "bufname('%')").strip()
                expected = {'tree':'agents', 'agents':'content', 'content':'tree'} if not right else {'content':'tree','tree':'agents','agents':'content'}
                if keys in ('<C-w>h','<C-w>l'):
                    want = 'tree' if (keys == '<C-w>l') == right else 'content'
                elif keys == '<C-w>j': want = 'agents'
                elif keys == '<C-w><C-k>': want = 'tree'
                elif keys == '<C-w>W': want = 'agents' # content's previous in window order
                else: want = expected[start]
                assert target == want, (right,start,keys,target,want)
                window_cases.append({'sidebar_right':right,'from':start,'keys':keys,'to':target})
        remote('--remote-expr', "execute('enew')")
        remote('--remote-expr', "luaeval('vim.fn.termopen({\"/bin/cat\"})')")
        terminal_modes = []
        for keys, want in [('i','t'), ('<C-\\><C-n>','n')]:
            remote('--remote-send', keys)
            deadline = time.monotonic() + 2
            while True:
                mode = remote('--remote-expr', 'mode()').strip()
                if mode == want: break
                if time.monotonic() >= deadline: raise AssertionError((keys,mode,want))
                time.sleep(.01)
            terminal_modes.append({'keys':keys,'mode':mode})
        result = {'scope': 'clean Neovim defaults; synthetic text; no personal config/plugins/shada',
                  'version': subprocess.check_output([binary, '--version'], env=env, timeout=4).decode().splitlines()[0],
                  'observations': observations, 'window_observations': window_cases, 'terminal_modes': terminal_modes, 'passed': True}
        receipt = stage / 'result.json'
        receipt.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
        print(json.dumps({'receipt': str(receipt), 'checks': len(observations)+len(window_cases)+len(terminal_modes), 'passed': True}))
    finally:
        try:
            remote('--remote-send', '<Esc>:qa!<CR>')
            process.wait(timeout=3)
        except (OSError, subprocess.SubprocessError):
            process.terminate()
            process.wait(timeout=3)


if __name__ == '__main__':
    main()
