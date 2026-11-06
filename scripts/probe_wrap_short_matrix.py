#!/usr/bin/env python3
"""Clean Neovim viewport reference for short synthetic lines; no user state."""
import argparse
import json
import random
import select
import socket
import subprocess
import tempfile
import threading
import time
from pathlib import Path

GROUPS = [((7, 10, 14), (0, 2, 4), (3, 8, 15)),
          ((6, 8, 12, 18), (0, 1, 3, 7), (2, 5, 9, 20))]

def pack(value):
    if type(value) is bool:
        return b'\xc3' if value else b'\xc2'
    if type(value) is int and 0 <= value < 128:
        return bytes([value])
    if type(value) is str:
        data = value.encode()
        if len(data) < 32:
            return bytes([0xa0 + len(data)]) + data
    if type(value) is list and len(value) < 16:
        return bytes([0x90 + len(value)]) + b''.join(map(pack, value))
    if type(value) is dict and len(value) < 16:
        return bytes([0x80 + len(value)]) + b''.join(pack(k) + pack(v) for k, v in value.items())
    raise ValueError('Unsupported UI attachment message')

def collect():
    cases = []
    nvim = '/opt/homebrew/bin/nvim'
    for heights, margins, counts in GROUPS:
        for height in heights:
            stage = Path(tempfile.mkdtemp(prefix='tw-wrap-short-matrix-', dir='/private/tmp'))
            stage.chmod(0o700)
            env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
                   'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_STATE_HOME': str(stage/'state'),
                   'XDG_DATA_HOME': str(stage/'data'), 'XDG_CACHE_HOME': str(stage/'cache'),
                   'NVIM_LOG_FILE': str(stage/'nvim.log')}
            rpc = stage/'rpc.sock'
            process = subprocess.Popen([nvim, '--headless', '-u', 'NONE', '-i', 'NONE', '--noplugin',
                                        '-n', '--listen', str(rpc)], env=env, cwd=stage,
                                       stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                       stderr=(stage/'stderr.log').open('wb'))
            ui = None
            worker = None
            stop = threading.Event()
            try:
                deadline = time.monotonic() + 8
                while not rpc.exists():
                    if time.monotonic() > deadline or process.poll() is not None:
                        raise RuntimeError('Neovim did not start: ' + str(stage))
                    time.sleep(.01)
                ui = socket.socket(socket.AF_UNIX)
                ui.connect(str(rpc))
                ui.sendall(pack([0, 1, 'nvim_ui_attach', [20, height, {'rgb': True, 'ext_linegrid': True}]]))
                def drain():
                    while not stop.is_set():
                        try:
                            ready, _, _ = select.select([ui], [], [], .1)
                            if ready and not ui.recv(65536):
                                break
                        except OSError:
                            break
                worker = threading.Thread(target=drain, daemon=True)
                worker.start()
                def remote(expression):
                    return subprocess.check_output([nvim, '--server', str(rpc), '--remote-expr', expression],
                                                   env=env, cwd=stage, stderr=subprocess.PIPE, timeout=5).decode()
                remote("execute('set wrap linebreak smoothscroll nonumber norelativenumber signcolumn=no tabstop=8 scrolljump=1')")
                rows = int(remote('winheight(0)'))
                for margin in margins:
                    remote("execute('set scrolloff=%d')" % margin)
                    for count in counts:
                        lines = ['line %02d' % i for i in range(count)]
                        remote("luaeval('vim.api.nvim_buf_set_lines(0,0,-1,false,_A)'," + json.dumps(lines) + ")")
                        remote("execute('normal! gg0')")
                        remote("screenpos(0,line('.'),col('.'))")
                        initial = json.loads(remote('json_encode(winsaveview())'))['topline'] - 1
                        rng = random.Random(height * 10000 + margin * 100 + count)
                        keys = ['gj'] * (count + 3) + ['gk'] * (count + 3)
                        keys += [rng.choice(['gj', 'gk', 'j', 'k', 'gg', 'G', str(rng.randint(1, count)) + 'G']) for _ in range(12)]
                        steps = []
                        for key in keys:
                            remote("execute('normal! %s')" % key)
                            remote("screenpos(0,line('.'),col('.'))")
                            cursor = int(remote("line('.')")) - 1
                            top = json.loads(remote('json_encode(winsaveview())'))['topline'] - 1
                            steps.append([key, cursor, top])
                        cases.append({'ui_height': height, 'rows': rows, 'scrolloff': margin,
                                      'line_count': count, 'initial_top': initial, 'steps': steps})
            finally:
                try:
                    subprocess.run([nvim, '--server', str(rpc), '--remote-send', '<Esc>:qa!<CR>'],
                                   env=env, cwd=stage, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, timeout=3, check=False)
                except (OSError, subprocess.TimeoutExpired):
                    pass
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                stop.set()
                if ui is not None:
                    ui.close()
                if worker is not None:
                    worker.join(timeout=1)
    version = subprocess.check_output([nvim, '--version'], env={'PATH': '/usr/bin:/bin'}, timeout=5).decode().splitlines()[0]
    return {'version': version, 'ui_width': 20, 'cases': cases}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record', action='store_true')
    args = parser.parse_args()
    result = collect()
    fixture = Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-wrap-short-matrix.json'
    if args.record:
        if fixture.is_symlink():
            raise RuntimeError('Refusing a symlink fixture')
        fixture.write_text(json.dumps(result, ensure_ascii=False, separators=(',', ':')) + '\n')
    elif json.loads(fixture.read_text()) != result:
        stage = Path(tempfile.mkdtemp(prefix='tw-wrap-short-difference-', dir='/private/tmp'))
        stage.chmod(0o700)
        (stage/'actual.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
        raise RuntimeError('Reference differs; evidence at ' + str(stage/'actual.json'))
    print(json.dumps({'cases': len(result['cases']), 'steps': sum(len(c['steps']) for c in result['cases']), 'recorded': args.record}))

if __name__ == '__main__':
    main()
