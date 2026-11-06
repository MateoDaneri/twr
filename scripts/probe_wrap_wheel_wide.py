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


def collect(extra=False, mixed=False, mode="normal", details=False, *, control=False,
            scroll_amount=3, scrolloff=4, compact=False, long_line_test=False):
    nvim = '/opt/homebrew/bin/nvim'
    cases = []
    geometries = (((17, 8), (35, 11), (61, 18)) if extra else ((13, 7), (27, 10), (43, 14)))
    if compact:
        geometries = ((13, 7), (27, 10))
    if long_line_test:
        geometries = ((13, 7),)
    for width, height in geometries:
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
            ui.sendall(pack([0, 1, 'nvim_ui_attach', [width, height, {'rgb': True, 'ext_linegrid': True}]]))

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

            remote("execute('set wrap linebreak smoothscroll nonumber norelativenumber signcolumn=no tabstop=8 scrolljump=1 scrolloff=" + str(scrolloff) + " mouse=a')")
            if scroll_amount != 3:
                remote("execute('set mousescroll=ver:" + str(scroll_amount) + ",hor:6')")
            if mode == "visual-block":
                remote("execute('set virtualedit=block')")
            rows = int(remote('winheight(0)'))
            for scenario in range(1 if long_line_test else (4 if compact else 6)):
                length = (21, 43, 79, 139, 319, 60)[scenario]
                pattern = ('ab ', 'é界 ', 'a\tb ', 'e\u0301x ')[scenario % 4]
                long_line = (pattern * (length // len(pattern) + 1))[:length]
                if scenario == 5:
                    lines = ['first', long_line, '', 'four', long_line, 'last']
                elif scenario % 2:
                    lines = [long_line, 'short', 'second', 'tail']
                else:
                    lines = ['first', 'second', long_line, 'end']
                if long_line_test:
                    lines = ['x' * 4000, 'last']
                if mode != "normal":
                    remote("nvim_input('<Esc>')")
                remote("luaeval('vim.api.nvim_buf_set_lines(0,0,-1,false,_A)'," + json.dumps(lines) + ")")
                remote("execute('normal! gg0')")
                if scenario % 3:
                    target = next(i for i, line in enumerate(lines) if line == long_line)
                    start_char = min(len(long_line) // 3, len(long_line) - 1)
                    byte_col = len(long_line[:start_char].encode('utf-8'))
                    remote("luaeval('vim.api.nvim_win_set_cursor(0,_A)'," + json.dumps([target + 1, byte_col]) + ")")
                if mode != "normal":
                    key = {"visual": "v", "visual-line": "V", "visual-block": "<C-v>", "insert": "i"}[mode]
                    remote("nvim_input('" + key + "')")
                    actual_mode = remote("mode()")
                    expected_mode = "\x16" if mode == "visual-block" else key
                    if not actual_mode.startswith(expected_mode):
                        raise RuntimeError('could not enter ' + mode + ' mode: ' + actual_mode)
                remote("screenpos(0,line('.'),col('.'))")
                initial = json.loads(remote('json_encode(winsaveview())'))
                initial_cursor = json.loads(remote('json_encode(getcurpos())'))
                rng = random.Random(height * 100 + scenario)
                keys = (['down'] * 3 + [rng.choice(['up', 'down']) for _ in range(24)] + ['up'] * 3) if mixed else (['down'] * 8 + ['up'] * 8)
                if compact:
                    keys = ['down'] * 4 + ['up'] * 4
                if long_line_test:
                    keys = ['down', 'up']
                steps = []
                for key in keys:
                    if control:
                        remote("nvim_input('" + str(scroll_amount) + ("<C-e>" if key == 'down' else "<C-y>") + "')")
                    else:
                        remote("luaeval('vim.api.nvim_input_mouse(\"wheel\",_A,\"\",0,2,2)'," + json.dumps(key) + ")")
                    remote("screenpos(0,line('.'),col('.'))")
                    cursor = json.loads(remote('json_encode(getcurpos())'))
                    view = json.loads(remote('json_encode(winsaveview())'))
                    step = {'key': key, 'cursor': cursor, 'topline': view['topline'], 'skipcol': view['skipcol']}
                    if mode != "normal":
                        step['mode'] = remote('mode()')
                        step['anchor'] = json.loads(remote("json_encode(getpos('v'))"))
                    if details:
                        step['virtcol'] = int(remote("virtcol('.')"))
                        step['virtspan'] = json.loads(remote("json_encode(virtcol('.', 1))"))
                        step['screen'] = json.loads(remote("json_encode(screenpos(0,line('.'),col('.')))"))
                    steps.append(step)
                case = {'ui_height': height, 'width': width, 'rows': rows, 'scrolloff': scrolloff, 'initial_cursor': initial_cursor,
                              'lines': lines, 'initial_topline': initial['topline'],
                              'initial_skipcol': initial['skipcol'], 'steps': steps}
                if compact:
                    case['scroll_amount'] = scroll_amount
                cases.append(case)
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
    return {'version': version, 'cases': cases}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record', action='store_true')
    parser.add_argument('--extra', action='store_true', help='use additional viewport sizes')
    parser.add_argument('--mixed', action='store_true', help='alternate scrolling directions')
    parser.add_argument('--mode', choices=('normal','visual','visual-line','visual-block','insert'), default='normal')
    parser.add_argument('--details', action='store_true', help='capture block virtual columns and screen positions')
    args = parser.parse_args()
    if (args.extra and args.mixed) or (args.mode not in ('normal', 'visual-block') and (args.extra or args.mixed)):
        parser.error('choose one of --extra, --mixed or a non-normal mode')
    if args.details and args.mode != 'visual-block':
        parser.error('--details requires --mode visual-block')
    data = collect(args.extra, args.mixed, args.mode, args.details)
    fixture_name = ('nvim-wrap-wheel-' + args.mode + '.json') if args.mode != 'normal' else ('nvim-wrap-wheel-mixed-directions.json' if args.mixed else ('nvim-wrap-wheel-extra-geometries.json' if args.extra else 'nvim-wrap-wheel-wide-unicode.json'))
    if args.details:
        fixture_name = ('nvim-wrap-wheel-block-extra-vspan.json' if args.extra else 'nvim-wrap-wheel-block-mixed-vspan.json' if args.mixed else 'nvim-wrap-wheel-block-vspan.json')
    fixture = Path(__file__).resolve().parents[1] / 'Resources/Fixtures' / fixture_name
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
