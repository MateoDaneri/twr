#!/usr/bin/env python3
"""Bounded, isolated Neovim input reference. No user config or task contents."""
import json
import subprocess
import tempfile
import time
from pathlib import Path

CASES = [
    ('insert-middle', 'abcd', ['<Left>', '<Left>', 'X']),
    ('delete-middle', 'abcd', ['<Home>', '<Right>', '<Del>']),
    ('backspace-middle', 'abcd', ['<Left>', '<BS>']),
    ('ctrl-h', 'abcd', ['<C-h>']),
    ('prefix-clear', 'foo bar tail', ['<Left>'] * 5 + ['<C-u>']),
    ('word-middle', 'foo bar tail', ['<Left>'] * 5 + ['<C-w>']),
    ('word-spaces', 'foo bar   ', ['<C-w>']),
    ('word-punctuation', 'foo.bar', ['<C-w>', '<C-w>']),
    ('word-underscore', 'foo_bar', ['<C-w>']),
    ('unicode-backspace', 'ñ😀', ['<BS>']),
    ('combining-backspace', 'xe\u0301', ['<BS>']),
    ('normal-end', 'abcd', ['<Esc>', '$']),
    ('normal-home-right', 'abcd', ['<Esc>', '0', 'l']),
    ('normal-delete', 'abcd', ['<Esc>', 'x']),
    ('normal-insert', 'abcd', ['<Esc>', 'h', 'i', 'X']),
    ('normal-append', 'abcd', ['<Esc>', 'h', 'a', 'X']),
    ('normal-I', 'abcd', ['<Esc>', 'I', 'X']),
    ('normal-A', 'abcd', ['<Esc>', '0', 'A', 'X']),
    ('empty-boundaries', '', ['<BS>', '<Del>', '<Home>', '<End>', '<Esc>', 'h', 'l', 'x']),
]


def main():
    stage = Path(tempfile.mkdtemp(prefix='tw-picker-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    binary = '/opt/homebrew/bin/nvim'
    env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
           'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_DATA_HOME': str(stage/'data'),
           'XDG_STATE_HOME': str(stage/'state'), 'XDG_CACHE_HOME': str(stage/'cache'),
           'NVIM_LOG_FILE': str(stage/'nvim.log')}
    socket = stage/'rpc.sock'
    process = subprocess.Popen([binary, '--headless', '-u', 'NONE', '-i', 'NONE',
                                '--noplugin', '-n', '--listen', str(socket),
                                '-c', 'set backspace=indent,eol,start'], env=env, cwd=stage,
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
        for label, query, keys in CASES:
            remote('--remote-send', '<Esc>')
            remote('--remote-expr', "execute('enew!')")
            remote('--remote-send', 'i' + query)
            observations = []
            for key in keys:
                remote('--remote-send', key)
                state = json.loads(remote('--remote-expr',
                    "json_encode({'mode':mode(),'text':getline(1),'cursor':col('.')-1})"))
                observations.append(state)
            results.append({'label': label, 'query': query, 'keys': keys,
                            'observations': observations})
        version = subprocess.check_output([binary, '--version'], env=env, cwd=stage,
                                          timeout=3).decode().splitlines()[0]
        data = {'version': version, 'config': 'NONE', 'shada': 'NONE', 'plugins': False,
                'backspace': 'indent,eol,start', 'scope': 'Plain Neovim input editing, not Snacks list mappings',
                'cases': results}
        output = stage/'results.json'
        output.write_text(json.dumps(data, ensure_ascii=False, indent=2)+'\n')
        baseline = Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-picker-input.json'
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
