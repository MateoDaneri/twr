#!/usr/bin/env python3
"""Run the experimental viewport model in an isolated, offline snapshot."""
import importlib.util
import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--zero-scrolloff', action='store_true')
parser.add_argument('--exact-byte', action='store_true')
parser.add_argument('--target-byte', action='store_true')
parser.add_argument('--scrolloff', type=int, choices=range(1, 11))
parser.add_argument('--sequential', action='store_true', help='carry modeled viewport and cursor across events')
parser.add_argument('--seed', type=int, default=0, help='independent deterministic fuzz seed')
parser.add_argument('--strict', action='store_true', help='fail on any modeled cursor, viewport, or coladd mismatch; requires --sequential --unseeded')
parser.add_argument('--unseeded', action='store_true', help='also carry modeled desired column (requires --sequential)')
parser.add_argument('--wheel-phase', action='store_true', help='experiment with the wheel model for the intermediate scrolloff=4 phase')
parser.add_argument('--follow-phase', action='store_true', help='also experimentally reveal an offscreen cursor after motion')
parser.add_argument('--mode', choices=('normal', 'visual', 'visual-line', 'visual-block'), default='normal')
parser.add_argument('--end-of-line', action='store_true')
group = parser.add_mutually_exclusive_group()
group.add_argument('--extra', action='store_true')
group.add_argument('--mixed', action='store_true')
group.add_argument('--fuzz', action='store_true')
args = parser.parse_args()
if args.seed < 0 or (args.seed and not args.fuzz):
    parser.error('--seed requires --fuzz and a nonnegative value')
if args.target_byte and (args.exact_byte or args.extra or args.mixed or args.fuzz):
    parser.error('--target-byte cannot be combined with --exact-byte, --extra, --mixed or --fuzz')
if args.zero_scrolloff and args.scrolloff is not None:
    parser.error('--zero-scrolloff and --scrolloff cannot be combined')
if args.strict and not (args.sequential and args.unseeded):
    parser.error('--strict requires --sequential --unseeded')
if args.unseeded and not args.sequential:
    parser.error('--unseeded requires --sequential')
if args.wheel_phase and (not args.sequential or args.zero_scrolloff or args.scrolloff not in (None, 4)):
    parser.error('--wheel-phase requires --sequential and scrolloff=4')
if args.follow_phase and not args.wheel_phase:
    parser.error('--follow-phase requires --wheel-phase')
if args.mode == 'visual-block' and not args.sequential:
    parser.error('--mode visual-block currently requires --sequential')
if args.end_of_line and args.mode != 'visual-block':
    parser.error('--end-of-line requires --mode visual-block')
root = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('rust_workspace', root/'scripts/rust_workspace.py')
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)
driver.prepare_build()
with driver.exclusive_build_lock():
    if shutil.disk_usage('/private/tmp').free < 2 * 1024**3:
        raise RuntimeError('Less than 2 GiB free; no build started')
    cache = Path(tempfile.mkdtemp(prefix='twr-half-page-model-', dir='/private/tmp'))
    cache.chmod(0o700)
    subprocess.run([sys.executable, '-I', '-S', str(root/'scripts/source_snapshot.py'),
                    str(root), str(cache), '--fresh'], check=True, timeout=180, stdout=subprocess.DEVNULL)
    stage = Path(json.loads((cache/'latest.json').read_text())['path'])
    if stage.parent != cache:
        raise RuntimeError('Snapshot escaped its private directory')
    if driver.native_fingerprint(stage)[0] != driver.native_fingerprint(root)[0]:
        raise RuntimeError('Snapshot does not match current source')
    editor = stage/'crates/workspace-app/src/editor_wrap_wheel.rs'
    with editor.open('a') as stream:
        stream.write('\n' + (root/'scripts/fixtures/wrap_half_page_view_model.rs').read_text())
    fixture = (('nvim-wrap-half-page' if args.mode == 'normal' else 'nvim-wrap-half-page-sync-' + args.mode)
               + ('-eol' if args.end_of_line else '')
               + ('-zero-scrolloff' if args.zero_scrolloff else f'-scrolloff-{args.scrolloff}' if args.scrolloff is not None else '')
               + ('-extra' if args.extra else '-mixed' if args.mixed else '-fuzz' if args.fuzz else '')
               + ('-exact-byte' if args.exact_byte else '') + ('-target-byte' if args.target_byte else '') + (f'-seed-{args.seed}' if args.seed else '') + '.json')
    shutil.copyfile(root/'Resources/Fixtures'/fixture,
                    stage/'Resources/Fixtures/nvim-wrap-half-page.json')
    env = driver.environment()
    env.update({'CARGO_BUILD_JOBS': '1', 'CARGO_INCREMENTAL': '0',
                'CARGO_NET_OFFLINE': 'true', 'TMPDIR': '/private/tmp',
                'TWR_HALF_PAGE_UNSEEDED': '1' if args.unseeded else '0',
                'TWR_HALF_PAGE_WHEEL_PHASE': '1' if args.wheel_phase else '0',
                'TWR_HALF_PAGE_FOLLOW_PHASE': '1' if args.follow_phase else '0',
                'TWR_HALF_PAGE_STRICT': '1' if args.strict else '0'})
    test = 'diagnostic_sequential_half_page' if args.sequential else 'compare_half_page_view_baseline'
    cmd = [str(driver.PINNED_BIN/'cargo'), 'test', '--locked', '-p', 'workspace-app',
           test, '--', '--nocapture']
    print('Research-only snapshot:', stage, flush=True)
    print('Strict selected-fixture check; this does not establish parity.' if args.strict else
          'The diagnostic reports mismatches; a passing test is not a parity claim.', flush=True)
    raise SystemExit(subprocess.run(cmd, cwd=stage, env=env, timeout=480, check=False).returncode)
