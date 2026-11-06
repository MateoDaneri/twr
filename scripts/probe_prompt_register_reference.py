#!/usr/bin/env python3
"""Single-line prompt register reference from synthetic isolated Neovim."""
import argparse
import json
import subprocess
import tempfile
import time
from pathlib import Path

CASES = [
    ('command-named', ':ab', [['<Left>'], ['<C-r>', 'a']]),
    ('command-unnamed', ':', [['<C-r>', '"']]),
    ('command-linewise', ':', [['<C-r>', 'c']]),
    ('clipboard-plus', ':', [['<C-r>', '+']]),
    ('clipboard-star', ':', [['<C-r>', '*']]),
    ('search-forward', '/', [['<C-r>', 'a']]),
    ('search-backward', '?', [['<C-r>', 'a']]),
    ('literal-single-line', ':', [['<C-r>', '<C-r>', 'a']]),
    ('empty-register', ':', [['<C-r>', 'z']]),
    ('escape-pending', ':ab', [['<C-r>', '<Esc>']]),
    ('control-c-pending', ':ab', [['<C-r>', '<C-c>']]),
]

def collect():
    stage = Path(tempfile.mkdtemp(prefix='tw-prompt-register-', dir='/private/tmp'))
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
                                       env=env, stderr=subprocess.PIPE, timeout=5).decode()
    def observe():
        return json.loads(remote('--remote-expr', "json_encode({'mode':mode(),'line':getcmdline(),'pos':getcmdpos()})"))
    try:
        deadline = time.monotonic() + 8
        while not socket.exists():
            if time.monotonic() > deadline or process.poll() is not None:
                raise RuntimeError('Neovim did not start: ' + str(stage))
            time.sleep(.01)
        remote('--remote-expr', '''luaeval("(function() vim.g.clipboard={name='fixture',copy={['+']=function() end,['*']=function() end},paste={['+']=function() return {'synthetic-clipboard'},'v' end,['*']=function() return {'synthetic-clipboard'},'v' end},cache_enabled=0}; vim.opt.clipboard='unnamedplus'; return 1 end)()")''')
        remote('--remote-expr', "setreg('a','alpha beta','v')")
        remote('--remote-expr', "setreg('\"','unnamed','v')")
        remote('--remote-expr', "setreg('c',['only'],'V')")
        results = []
        for label, prefix, operations in CASES:
            remote('--remote-send', '<Esc>')
            remote('--remote-send', prefix)
            observations = [observe()]
            for operation in operations:
                # Neovim waits for the register name, so send each complete operation.
                remote('--remote-send', ''.join(operation))
                observations.append(observe())
            results.append({'label': label, 'prefix': prefix, 'operations': operations, 'observations': observations})
        version = subprocess.check_output([nvim, '--version'], cwd=stage, env=env, timeout=4).decode().splitlines()[0]
        return {'version': version, 'cases': results}, stage
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
    fixture = Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-prompt-register.json'
    if args.record:
        if fixture.is_symlink():
            raise RuntimeError('Refusing a symlink fixture')
        fixture.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    elif json.loads(fixture.read_text()) != result:
        (stage/'results.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
        raise RuntimeError('Reference differs; evidence at ' + str(stage/'results.json'))
    print(json.dumps({'reference_stage': str(stage), 'cases': len(result['cases']), 'recorded': args.record}))

if __name__ == '__main__':
    main()
