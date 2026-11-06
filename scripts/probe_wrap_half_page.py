#!/usr/bin/env python3
"""Reference wrapped half-page scrolling on synthetic lines in clean Neovim.

No personal configuration, clipboard, sessions, or files are used. Without
--record, compare the complete trace with the checked-in reference.
"""
import argparse
import copy
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


def collect(extra=False, mixed=False, mode="normal", details=False, scrolloff=4, synchronous=False, redraw=False, end_of_line=False, fuzz=False, heights=False, exact_byte=False, target_byte=False, seed=0, columns=False, phase=False, phase_motion=False, phase_redraw=False, phase_restore=False, phase_target=None):
    nvim = '/opt/homebrew/bin/nvim'
    cases = []
    for width, height in (((13, 7),) if phase_target == 'seed3-up' else ((27, 10),) if phase_target == 'seed3-second' or target_byte else ((17, 8), (35, 11), (61, 18)) if extra else ((13, 7), (27, 10), (43, 14))):
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

            remote("execute('set wrap linebreak smoothscroll nostartofline nonumber norelativenumber signcolumn=no tabstop=8 scrolljump=1 scrolloff=" + str(scrolloff) + " mouse=a')")
            if mode == "visual-block":
                remote("execute('set virtualedit=block')")
            rows = int(remote('winheight(0)'))
            for scenario_index in range(6):
                scenario = 2 if target_byte else scenario_index
                if phase_target and scenario != (4 if phase_target == 'seed3-up' else 1):
                    continue
                # 'scroll' is window-local and a count changes it. Reset it for
                # each independent case rather than inheriting the previous one.
                remote("execute('setlocal scroll=0')")
                rng = random.Random(width * 10000 + height * 100 + scenario + 731 + seed * 1000003)
                length = rng.randint(8, 340) if fuzz else (21, 43, 79, 139, 319, 60)[scenario]
                pattern = (('x e\u0301界\t', 'one two-three  ', 'β\t日 a ', '  short-long\t')[scenario % 4]
                           if fuzz else ('ab ', 'é界 ', 'a\tb ', 'e\u0301x ')[scenario % 4])
                long_line = (pattern * (length // len(pattern) + 1))[:length]
                if scenario == 5:
                    lines = ['first', long_line, '', 'four', long_line, 'last']
                elif scenario % 2:
                    lines = [long_line, 'short', 'second', 'tail']
                else:
                    lines = ['first', 'second', long_line, 'end']
                if target_byte:
                    lines[0] = ("firs", "first", "firsts", "éabc", "界ab", "e\u0301ab")[scenario_index]
                if exact_byte:
                    # Independent destinations around the observed byte boundary,
                    # including multibyte and combining characters.
                    destination = next(i for i, line in enumerate(lines) if line != long_line)
                    lines[destination] = (('firs', 'first', 'firsts')[(13, 27, 43).index(width)] if scenario == 2 and width in (13, 27, 43) else ('firs', 'first', 'firsts', 'éab', '界ab', 'e\u0301ab')[scenario])
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
                if end_of_line:
                    initial_anchor = json.loads(remote("json_encode(getpos('v'))"))
                    remote("luaeval('vim.api.nvim_feedkeys(\"$\",\"xt\",false)')")
                remote("screenpos(0,line('.'),col('.'))")
                initial = json.loads(remote('json_encode(winsaveview())'))
                initial_cursor = json.loads(remote('json_encode(getcurpos())'))
                keys = ['<C-d>', '<C-d>', '<C-u>', '2<C-d>', '<C-u>', '<C-u>',
                        '5<C-u>', '<C-d>', '99<C-d>', '<C-u>']
                if mixed or fuzz:
                    rng = random.Random(width * 1000 + height * 10 + scenario + seed * 1000003)
                    keys = [rng.choice(['<C-d>', '<C-u>', '1<C-d>', '3<C-u>', '99<C-d>']) for _ in range(10)]
                steps = []
                for step_index, key in enumerate(keys):
                    phase_state = None
                    ordinary_phase = (width == 27 and ((seed == 1 and scenario == 5 and step_index == 2) or (seed == 2 and scenario == 1 and step_index == 9) or (seed == 3 and scenario == 1 and step_index == 0)))
                    target_phase = ((phase_target == 'seed3-up' and step_index == 7) or (phase_target == 'seed3-second' and step_index == 1))
                    if phase and (target_phase if phase_target else ordinary_phase):
                        saved = json.loads(remote('json_encode(winsaveview())'))
                        phase_key = '1<C-y>' if phase_target == 'seed3-up' else '3<C-e>' if phase_target == 'seed3-second' else '1<C-e>' if seed == 2 else '8<C-e>'
                        remote("luaeval('vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes(_A,true,false,true),\"xt\",false)'," + json.dumps(phase_key) + ")")
                        phase_state = {'view': json.loads(remote('json_encode(winsaveview())')),
                                       'cursor': json.loads(remote('json_encode(getcurpos())'))}
                        if phase_motion:
                            interim = dict(phase_state['view'])
                            interim.update({key: saved[key] for key in ('lnum', 'col', 'coladd', 'curswant')})
                            remote("luaeval('vim.fn.winrestview(_A)'," + json.dumps(interim) + ")")
                            if phase_restore:
                                phase_state['after_restore'] = {'view': json.loads(remote('json_encode(winsaveview())')),
                                                                'cursor': json.loads(remote('json_encode(getcurpos())'))}
                            motion = '1gk' if phase_target == 'seed3-up' else '1gj' if seed == 2 else '8gj'
                            remote("luaeval('vim.api.nvim_feedkeys(_A,\"xt\",false)'," + json.dumps(motion) + ")")
                            if phase_redraw:
                                remote("execute('redraw!')")
                                remote("screenpos(0,line('.'),col('.'))")
                            phase_state['after_' + motion] = {'view': json.loads(remote('json_encode(winsaveview())')),
                                                        'cursor': json.loads(remote('json_encode(getcurpos())'))}
                        remote("luaeval('vim.fn.winrestview(_A)'," + json.dumps(saved) + ")")
                    if synchronous:
                        remote("luaeval('vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes(_A,true,false,true),\"xt\",false)'," + json.dumps(key) + ")")
                    else:
                        remote("nvim_input(" + json.dumps(key) + ")")
                    if redraw:
                        remote("execute('redraw')")
                    remote("screenpos(0,line('.'),col('.'))")
                    cursor = json.loads(remote('json_encode(getcurpos())'))
                    view = json.loads(remote('json_encode(winsaveview())'))
                    step = {'key': key, 'cursor': cursor, 'topline': view['topline'],
                            'skipcol': view['skipcol'], 'scroll': int(remote('&l:scroll'))}
                    if phase_state is not None:
                        step['phase_ctrl_e'] = phase_state
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
                if details:
                    case['startofline'] = bool(int(remote('&startofline')))
                if heights:
                    case['line_heights'] = [int(remote("luaeval('vim.api.nvim_win_text_height(0,{start_row=_A,end_row=_A}).all'," + str(i) + ")")) for i in range(len(lines))]
                if columns:
                    case['line_columns'] = [int(remote("virtcol([" + str(i + 1) + ",'$'])")) for i in range(len(lines))]
                    case['plain_widths'] = [int(remote('strdisplaywidth(' + json.dumps(line) + ')')) for line in lines]
                if end_of_line:
                    case['initial_anchor'] = initial_anchor
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
    parser.add_argument('--seed', type=int, default=0, help='independent deterministic fuzz seed')
    parser.add_argument('--exact-byte', action='store_true', help='vary destination byte lengths around EOL')
    parser.add_argument('--target-byte', action='store_true', help='replay the observed boundary with different destinations')
    parser.add_argument('--zero-scrolloff', action='store_true', help='isolate cursor correction effects')
    parser.add_argument('--scrolloff', type=int, choices=range(1, 11), help='record a nondefault scrolloff value')
    parser.add_argument('--synchronous', action='store_true', help='execute each key sequence before reading state')
    parser.add_argument('--details', action='store_true', help='capture cursor virtual and screen positions')
    parser.add_argument('--redraw', action='store_true', help='redraw before reading each event')
    parser.add_argument('--heights', action='store_true', help='capture Neovim screen-line heights')
    parser.add_argument('--columns', action='store_true', help='capture EOL virtual columns and plain display widths')
    parser.add_argument('--phase', action='store_true', help='probe one intermediate scroll in the synthetic seed-1 case')
    parser.add_argument('--phase-motion', action='store_true', help='probe a restored cursor followed by 8gj; requires --phase')
    parser.add_argument('--phase-redraw', action='store_true', help='force redraw after phase motion; requires --phase-motion')
    parser.add_argument('--phase-restore', action='store_true', help='observe view immediately after restoring cursor; requires --phase-motion')
    parser.add_argument('--phase-target', choices=('seed3-second', 'seed3-up'), help='isolate one seed-3 motion and its intermediate phase')
    parser.add_argument('--mode', choices=('normal', 'visual', 'visual-line', 'visual-block'), default='normal')
    parser.add_argument('--end-of-line', action='store_true', help='start Visual Block at $')
    group = parser.add_mutually_exclusive_group()
    group.add_argument('--extra', action='store_true', help='test additional viewport sizes')
    group.add_argument('--mixed', action='store_true', help='test mixed counted directions')
    group.add_argument('--fuzz', action='store_true', help='test additional deterministic synthetic text and key sequences')
    args = parser.parse_args()
    if args.end_of_line and args.mode != 'visual-block':
        parser.error('--end-of-line requires --mode visual-block')
    if args.phase_motion and not args.phase:
        parser.error('--phase-motion requires --phase')
    if args.phase_redraw and not args.phase_motion:
        parser.error('--phase-redraw requires --phase-motion')
    if args.phase_restore and not args.phase_motion:
        parser.error('--phase-restore requires --phase-motion')
    if args.phase_target and not (args.phase and args.phase_motion and args.phase_restore and args.fuzz and args.seed == 3 and args.mode == 'normal' and not any((args.extra, args.mixed, args.details, args.redraw, args.synchronous, args.heights, args.exact_byte, args.target_byte, args.columns, args.zero_scrolloff)) and args.scrolloff in (None, 4)):
        parser.error('--phase-target requires --phase --phase-motion --phase-restore --fuzz --seed 3 and the default normal-mode fixture options')
    if args.phase and not (args.fuzz and args.seed in (1, 2, 3) and args.mode == 'normal'):
        parser.error('--phase requires --fuzz --seed 1, 2 or 3 and normal mode')
    if args.seed < 0 or (args.seed and not args.fuzz):
        parser.error('--seed requires --fuzz and a nonnegative value')
    if args.target_byte and (args.exact_byte or args.extra or args.mixed or args.fuzz):
        parser.error('--target-byte cannot be combined with --exact-byte, --extra, --mixed or --fuzz')
    if args.zero_scrolloff and args.scrolloff is not None:
        parser.error('--zero-scrolloff and --scrolloff cannot be combined')
    scrolloff = 0 if args.zero_scrolloff else (args.scrolloff if args.scrolloff is not None else 4)
    data = collect(extra=args.extra, mixed=args.mixed, mode=args.mode, scrolloff=scrolloff, synchronous=args.synchronous, details=args.details, redraw=args.redraw, end_of_line=args.end_of_line, fuzz=args.fuzz, heights=args.heights, exact_byte=args.exact_byte, target_byte=args.target_byte, seed=args.seed, columns=args.columns, phase=args.phase, phase_motion=args.phase_motion, phase_redraw=args.phase_redraw, phase_restore=args.phase_restore, phase_target=args.phase_target)
    if args.phase_target:
        original = json.loads((Path(__file__).resolve().parents[1] / 'Resources/Fixtures/nvim-wrap-half-page-fuzz-seed-3.json').read_text())
        clean = copy.deepcopy(data['cases'][0])
        for step in clean['steps']:
            step.pop('phase_ctrl_e', None)
        index = 4 if args.phase_target == 'seed3-up' else 7
        if data['version'] != original['version'] or clean != original['cases'][index]:
            raise RuntimeError('The phase probe changed the original final trace')
    option_suffix = '-zero-scrolloff' if args.zero_scrolloff else (f'-scrolloff-{args.scrolloff}' if args.scrolloff is not None else '')
    name = ('nvim-wrap-half-page-sync' if args.synchronous else 'nvim-wrap-half-page') + ('-' + args.mode if args.mode != 'normal' else '') + ('-eol' if args.end_of_line else '') + option_suffix + ('-extra' if args.extra else '-mixed' if args.mixed else '-fuzz' if args.fuzz else '') + ('-details' if args.details else '') + ('-redraw' if args.redraw else '') + ('-heights' if args.heights else '') + ('-exact-byte' if args.exact_byte else '') + ('-target-byte' if args.target_byte else '') + (f'-seed-{args.seed}' if args.seed else '') + ('-columns' if args.columns else '') + ('-phase' if args.phase else '') + ('-motion' if args.phase_motion else '') + '.json'
    if args.phase_redraw:
        name = name.removesuffix('.json') + '-redraw.json'
    if args.phase_restore:
        name = name.removesuffix('.json') + '-restore.json'
    if args.phase_target:
        name = name.removesuffix('.json') + '-' + args.phase_target + '.json'
    fixture = Path(__file__).resolve().parents[1] / 'Resources/Fixtures' / name
    if args.record:
        if fixture.is_symlink():
            raise RuntimeError('Refusing a symlink fixture')
        fixture.write_text(json.dumps(data, ensure_ascii=False, separators=(',', ':')) + '\n')
    elif json.loads(fixture.read_text()) != data:
        stage = Path(tempfile.mkdtemp(prefix='tw-wrap-half-page-difference-', dir='/private/tmp'))
        stage.chmod(0o700)
        (stage/'actual.json').write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')
        raise RuntimeError('Reference differs; evidence at ' + str(stage/'actual.json'))
    print(json.dumps({'cases': len(data['cases']), 'steps': sum(len(x['steps']) for x in data['cases']), 'recorded': args.record}))


if __name__ == '__main__':
    main()
