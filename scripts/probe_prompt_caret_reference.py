#!/usr/bin/env python3
"""Reference prompt caret behavior against isolated clean Neovim; no user state."""
import argparse
import json
import subprocess
import tempfile
import time
from pathlib import Path

CASES = []
for prefix in (':', '/', '?'):
    for label, query, keys in [
        ('edit-mid', 'abc', ['<Left>', '<Left>', 'X', '<Del>', '<Home>', '<End>']),
        ('unicode', 'á界z', ['<Left>', '<Left>', 'X', '<BS>', '<Home>', '<Right>', '<C-u>']),
        ('combining', 'ae\u0301z', ['<Left>', '<Left>', '<Right>', '<Del>']),
        ('combining-insert', 'ae\u0301z', ['<Home>', '<Right>', '\u0301', '<Left>', '<Right>', '<BS>']),
        ('emoji', 'a👩\u200d💻z', ['<Left>', '<Left>', '<Right>', '<BS>']),
        ('word-spaces-mid', 'one two   three', ['<Left>', '<Left>', '<Left>', '<Left>', '<Left>', '<C-w>']),
        ('word-mid', 'one two', ['<Left>', '<Left>', '<C-w>']),
        ('word-arrows', 'foo.bar  baz-qux', ['<S-Left>', '<S-Left>', '<S-Left>', '<S-Right>', '<C-Right>', '<C-Left>']),
        ('word-unicode', 'á界  😀x', ['<C-Left>', '<C-Left>', '<C-Left>', '<C-Right>', '<S-Right>', '<S-Right>']),
        ('word-nbsp', 'foo\u00a0bar', ['<C-Left>', '<C-Right>', '<S-Left>', '<S-Right>']),
        ('word-ideographic-space', 'foo\u3000bar', ['<C-Left>', '<C-Right>', '<S-Left>', '<S-Right>']),
        ('word-edges', '   abc   ', ['<Home>', '<C-Left>', '<C-Right>', '<C-Right>', '<C-Right>', '<C-Right>', '<C-Right>', '<C-Left>']),
        ('backspace-start', 'abc', ['<Home>', '<BS>', '<Right>', '<BS>']),
        ('controls', 'abcdef', ['<C-b>', '<C-e>', '<Left>', '<C-u>']),
        ('empty', '', ['<C-u>', '<C-w>', '<BS>']),
    ]:
        CASES.append({'label': prefix + label, 'prefix': prefix, 'query': query, 'keys': keys})


def collect():
    stage = Path(tempfile.mkdtemp(prefix='tw-prompt-caret-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    nvim = '/opt/homebrew/bin/nvim'
    env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
           'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_STATE_HOME': str(stage/'state'),
           'XDG_DATA_HOME': str(stage/'data'), 'XDG_CACHE_HOME': str(stage/'cache'),
           'NVIM_LOG_FILE': str(stage/'nvim.log')}
    socket = stage/'socket'
    process = subprocess.Popen([nvim, '--headless', '-u', 'NONE', '-i', 'NONE', '--noplugin',
                                '-n', '--listen', str(socket)], cwd=stage, env=env,
                               stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                               stderr=(stage/'stderr.log').open('wb'))
    def remote(*args):
        return subprocess.check_output([nvim, '--server', str(socket), *args], cwd=stage,
                                       env=env, stderr=subprocess.PIPE, timeout=4).decode()
    def observe():
        return json.loads(remote('--remote-expr', "json_encode({'mode':mode(),'line':getcmdline(),'pos':getcmdpos()})"))
    try:
        deadline = time.monotonic() + 5
        while not socket.exists():
            if time.monotonic() > deadline or process.poll() is not None:
                raise RuntimeError('Neovim did not start: ' + str(stage))
            time.sleep(.01)
        results = []
        for case in CASES:
            remote('--remote-send', '<Esc>')
            remote('--remote-send', case['prefix'] + case['query'])
            observations = [observe()]
            for key in case['keys']:
                remote('--remote-send', key)
                observations.append(observe())
            results.append({**case, 'observations': observations})
        version = subprocess.check_output([nvim, '--version'], cwd=stage, env=env, timeout=4).decode().splitlines()[0]
        output = {'version': version, 'config': 'NONE', 'shada': 'NONE', 'plugins': False, 'cases': results}
        (stage/'results.json').write_text(json.dumps(output, ensure_ascii=False, indent=2) + '\n')
        return output, stage
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record', action='store_true')
    args = parser.parse_args()
    result, stage = collect()
    fixture = Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-prompt-caret.json'
    if args.record:
        if fixture.is_symlink():
            raise RuntimeError('Refusing a symlink fixture')
        fixture.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    elif json.loads(fixture.read_text()) != result:
        raise RuntimeError('Reference differs; evidence at ' + str(stage/'results.json'))
    print(json.dumps({'reference': str(stage/'results.json'), 'cases': len(result['cases']), 'recorded': args.record}))


if __name__ == '__main__':
    main()
