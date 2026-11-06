#!/usr/bin/env python3
"""Reference wrapped mouse-wheel behavior on synthetic lines in clean Neovim.

No personal configuration, clipboard, sessions, or files are used. Without
--record, compare the complete trace with the checked-in reference.
"""
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


def pack(value):
    if type(value) is bool:
        return b'\xc3' if value else b'\xc2'
    if type(value) is int and 0 <= value < 128:
        return bytes([value])
    if type(value) is str:
        encoded = value.encode()
        if len(encoded) < 32:
            return bytes([0xa0 + len(encoded)]) + encoded
    if type(value) is list and len(value) < 16:
        return bytes([0x90 + len(value)]) + b''.join(map(pack, value))
    if type(value) is dict and len(value) < 16:
        return bytes([0x80 + len(value)]) + b''.join(pack(k) + pack(v) for k, v in value.items())
    raise ValueError('Unsupported UI attachment message')


def collect():
    nvim = '/opt/homebrew/bin/nvim'
    cases = []
    for height in (7, 10, 14):
        stage = Path(tempfile.mkdtemp(prefix='tw-wrap-mixed-', dir='/private/tmp'))
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

            def remote(expr):
                return subprocess.check_output([nvim, '--server', str(rpc), '--remote-expr', expr],
                                               env=env, cwd=stage, stderr=subprocess.PIPE, timeout=5).decode()

            remote("execute('set wrap linebreak smoothscroll nonumber norelativenumber signcolumn=no tabstop=8 scrolljump=1 scrolloff=4 mouse=a')")
            rows = int(remote('winheight(0)'))
            for scenario in range(6):
                length = (21, 43, 79, 139, 319, 60)[scenario]
                long_line = ('ab ' * ((length + 2) // 3))[:length]
                if scenario == 5:
                    lines = ['first', long_line, '', 'four', long_line, 'last']
                elif scenario % 2:
                    lines = [long_line, 'short', 'second', 'tail']
                else:
                    lines = ['first', 'second', long_line, 'end']
                remote("luaeval('vim.api.nvim_buf_set_lines(0,0,-1,false,_A)'," + json.dumps(lines) + ")")
                remote("execute('normal! gg0')")
                remote("screenpos(0,line('.'),col('.'))")
                initial = json.loads(remote('json_encode(winsaveview())'))
                rng = random.Random(height * 100 + scenario)
                keys = ['down'] * 8 + ['up'] * 8
                steps = []
                for key in keys:
                    remote("luaeval('vim.api.nvim_input_mouse(\"wheel\",_A,\"\",0,2,2)'," + json.dumps(key) + ")")
                    remote("screenpos(0,line('.'),col('.'))")
                    cursor = json.loads(remote('json_encode(getcurpos())'))
                    view = json.loads(remote('json_encode(winsaveview())'))
                    steps.append({'key': key, 'cursor': cursor, 'topline': view['topline'], 'skipcol': view['skipcol']})
                cases.append({'ui_height': height, 'rows': rows, 'scrolloff': 4,
                              'lines': lines, 'initial_topline': initial['topline'],
                              'initial_skipcol': initial['skipcol'], 'steps': steps})
        finally:
            try:
                subprocess.run([nvim, '--server', str(rpc), '--remote-send', '<Esc>:qa!<CR>'], env=env,
                               cwd=stage, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               timeout=3, check=False)
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
    data = collect()
    fixture = Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-wrap-wheel-matrix.json'
    if args.record:
        if fixture.is_symlink():
            raise RuntimeError('Refusing a symlink fixture')
        fixture.write_text(json.dumps(data, ensure_ascii=False, separators=(',', ':')) + '\n')
    elif json.loads(fixture.read_text()) != data:
        stage = Path(tempfile.mkdtemp(prefix='tw-wrap-mixed-difference-', dir='/private/tmp'))
        stage.chmod(0o700)
        (stage/'actual.json').write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')
        raise RuntimeError('Reference differs; evidence at ' + str(stage/'actual.json'))
    print(json.dumps({'cases': len(data['cases']), 'steps': sum(len(x['steps']) for x in data['cases']), 'recorded': args.record}))


if __name__ == '__main__':
    main()
