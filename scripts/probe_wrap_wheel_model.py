#!/usr/bin/env python3
"""Run the incomplete wheel model against synthetic Neovim traces.

Builds an isolated source snapshot with the project-local toolchain, offline,
under the project's exclusive build lock. Expected to fail until parity is
implemented. Never edits the checkout, profiles, tmux sessions, or live app.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--wide', action='store_true', help='audit Unicode, tabs and varying widths')
    parser.add_argument('--extra', action='store_true', help='audit additional viewport sizes (implies --wide)')
    parser.add_argument('--mixed', action='store_true', help='audit alternating scroll directions (implies --wide)')
    parser.add_argument('--perf', action='store_true', help='measure experimental model cost (implies --wide)')
    parser.add_argument('--properties', action='store_true', help='check position order and cache bound (implies --wide)')
    parser.add_argument('--mode', choices=('normal','visual','visual-line','visual-block','insert'), default='normal')
    args = parser.parse_args()
    if (args.extra and args.mixed) or ((args.extra or args.mixed) and (args.perf or args.properties)) or ((args.extra or args.mixed) and args.mode not in ('normal', 'visual-block')) or (args.mode != 'normal' and (args.perf or args.properties)) or (args.perf and args.properties):
        parser.error('choose at most one of --extra, --mixed, --perf, --properties')
    args.wide |= args.extra or args.mixed or args.perf or args.properties or args.mode != 'normal'
    spec = importlib.util.spec_from_file_location('rust_workspace', ROOT / 'scripts/rust_workspace.py')
    driver = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(driver)
    driver.prepare_build()
    with driver.exclusive_build_lock():
        if shutil.disk_usage('/private/tmp').free < 2 * 1024**3:
            raise RuntimeError('Less than 2 GiB free; no build started')
        cache = Path(tempfile.mkdtemp(prefix='twr-wheel-model-', dir='/private/tmp'))
        cache.chmod(0o700)
        subprocess.run([sys.executable, '-I', '-S', str(ROOT/'scripts/source_snapshot.py'),
                        str(ROOT), str(cache), '--fresh'], check=True, timeout=180,
                       stdout=subprocess.DEVNULL)
        stage = Path(json.loads((cache/'latest.json').read_text())['path'])
        if stage.parent != cache:
            raise RuntimeError('Snapshot escaped its private directory')
        editor = stage/'crates/workspace-app/src/editor_wrap.rs'
        with editor.open('a') as stream:
            model = 'wrap_wheel_block_model.rs' if args.mode == 'visual-block' else ('wrap_wheel_wide_model.rs' if args.wide else 'wrap_wheel_model.rs')
            stream.write('\n' + (ROOT/'scripts/fixtures'/model).read_text())
        fixture = 'nvim-wrap-wheel-block-vspan.json' if args.mode == 'visual-block' else ('nvim-wrap-wheel-wide-unicode.json' if args.wide else 'nvim-wrap-wheel-matrix.json')
        source_fixture = (('nvim-wrap-wheel-block-extra-vspan.json' if args.extra else 'nvim-wrap-wheel-block-mixed-vspan.json' if args.mixed else fixture) if args.mode == 'visual-block' else ('nvim-wrap-wheel-' + args.mode + '.json') if args.mode != 'normal' else ('nvim-wrap-wheel-mixed-directions.json' if args.mixed else ('nvim-wrap-wheel-extra-geometries.json' if args.extra else fixture)))
        shutil.copyfile(ROOT/'Resources/Fixtures'/source_fixture, stage/'Resources/Fixtures'/fixture)
        if args.wide:
            shutil.copyfile(ROOT/'Resources/Fixtures/nvim-wrap-wheel-matrix.json', stage/'Resources/Fixtures/nvim-wrap-wheel-matrix.json')
        env = driver.environment()
        env.update({'CARGO_BUILD_JOBS': '1', 'CARGO_INCREMENTAL': '0',
                    'CARGO_NET_OFFLINE': 'true', 'TMPDIR': '/private/tmp'})
        command = [str(driver.PINNED_BIN/'cargo'), 'test', '--locked', '-p', 'workspace-app',
                   'diagnostic_wheel_cost' if args.perf else ('wheel_cache_properties' if args.properties else 'compare_wheel_reference'), '--', '--nocapture']
        print('Isolated research snapshot:', stage, flush=True)
        print('A match covers these synthetic cases only; the model is not production code.', flush=True)
        return subprocess.run(command, cwd=stage, env=env, timeout=480, check=False).returncode


if __name__ == '__main__':
    raise SystemExit(main())
