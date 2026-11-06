#!/usr/bin/env python3
"""Bounded, isolated Neovim window focus reference. No user config or task contents."""
import json
import subprocess
import tempfile
import time
from pathlib import Path

CASES = [
    ('count-before', [['2', '<C-w>', 'w'], ['1', '<C-w>', 'W'], ['3', '<C-w>', '<C-w>']]),
    ('count-after', [['<C-w>', '2', 'w'], ['<C-w>', '1', 'W'], ['<C-w>', '3', '<C-w>']]),
    ('count-product', [['2', '<C-w>', '2', 'w'], ['2', '<C-w>', '1', 'w']]),
    ('count-clamp', [['9', '<C-w>', 'W'], ['<C-w>', '1', '0', 'w']]),
    ('count-ignored', [['2', '<C-w>', 'b'], ['2', '<C-w>', 't'], ['2', '<C-w>', 'p']]),
    ('delayed-prefix', [['<C-w>', 'w']]),
]




def main():
    stage = Path(tempfile.mkdtemp(prefix='tw-window-count-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    binary = '/opt/homebrew/bin/nvim'
    env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
           'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_DATA_HOME': str(stage/'data'),
           'XDG_STATE_HOME': str(stage/'state'), 'XDG_CACHE_HOME': str(stage/'cache'),
           'NVIM_LOG_FILE': str(stage/'nvim.log')}
    socket = stage/'rpc.sock'
    process = subprocess.Popen([binary, '--headless', '-u', 'NONE', '-i', 'NONE',
                                '--noplugin', '-n', '--listen', str(socket),
                                '-c', 'set splitright splitbelow'], env=env, cwd=stage,
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
            remote('--remote-expr', "execute('only! | enew! | vnew | new | wincmd t')")
            observations = []
            for command in commands:
                if label == 'delayed-prefix':
                    remote('--remote-send', command[0])
                    time.sleep(2.2)
                    remote('--remote-send', ''.join(command[1:]))
                else:
                    remote('--remote-send', ''.join(command))
                observations.append(json.loads(remote('--remote-expr',
                    "json_encode({'mode':mode(),'window':winnr(),'windows':winnr('$')})")))
            results.append({'label': label, 'commands': commands, 'observations': observations})
        version = subprocess.check_output([binary, '--version'], env=env, cwd=stage,
                                          timeout=3).decode().splitlines()[0]
        data = {'version': version, 'config': 'NONE', 'shada': 'NONE', 'plugins': False,
                'layout': 'content left, tree above agents on right; three distinct buffers',
                'scope': 'Neovim window focus, no tmux or GUI',
                'cases': results}
        output = stage/'results.json'
        output.write_text(json.dumps(data, ensure_ascii=False, indent=2)+'\n')
        baseline = Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-window-count.json'
        if baseline.exists() and json.loads(baseline.read_text()) != data:
            raise RuntimeError('Reference changed; retained evidence at ' + str(output))
        print(json.dumps({'reference': str(output), 'cases': len(results), 'baseline_compared': baseline.exists()}))
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
