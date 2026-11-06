#!/usr/bin/env python3
"""Run real WorkspaceView event tests on GPUI's synthetic platform.

The test build is isolated from the installed app, user profiles and sessions.
It does not exercise AppKit, the macOS IME, or the real screen.
"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
import tempfile

sys.dont_write_bytecode = True
import rust_workspace as driver
import source_snapshot

ROOT = Path(__file__).resolve().parent.parent
TEST_SOURCE = ROOT / 'scripts/gpui_window_tests.rs'
TEST_LOCK = ROOT / 'scripts/gpui-window-tests.lock'
GIB = 1024 ** 3


def packages(path):
    """Compare pinned package identities without importing user Cargo config."""
    found = {}
    for block in path.read_text().split('[[package]]')[1:]:
        def field(name):
            match = re.search(r'^' + re.escape(name) + r' = "([^"\n]+)"$', block, re.M)
            return match.group(1) if match else ''
        identity = (field('name'), field('version'), field('source'))
        if not identity[0] or not identity[1] or identity in found:
            raise RuntimeError('Invalid or duplicate package in lockfile')
        deps = re.search(r'^dependencies = \[(.*?)^\]', block, re.M | re.S)
        dependencies = re.findall(r'^\s*"([^"\n]+)",?$', deps.group(1), re.M) if deps else []
        found[identity] = (field('checksum'), dependencies)
    if not found:
        raise RuntimeError('Empty lockfile')
    return found


def resolved_dependencies(raw, packages_by_identity):
    resolved = set()
    for value in raw:
        parts = value.split(' ', 2)
        name = parts[0]
        version = parts[1] if len(parts) > 1 else None
        source = parts[2][1:-1] if len(parts) > 2 and parts[2].startswith('(') and parts[2].endswith(')') else None
        matches = [key for key in packages_by_identity if key[0] == name
                   and (version is None or key[1] == version)
                   and (source is None or key[2] == source)]
        if len(matches) != 1:
            raise RuntimeError('Ambiguous package dependency in test lock')
        resolved.add(matches[0])
    return resolved


def lock_preserves_baseline(baseline, testing):
    for key, (checksum, dependencies) in baseline.items():
        target = testing.get(key)
        if target is None or target[0] != checksum:
            return False
        if not resolved_dependencies(dependencies, baseline).issubset(
            resolved_dependencies(target[1], testing)
        ):
            return False
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ghostty-stage', type=Path, help='Verified private Ghostty stage')
    parser.add_argument('--repeat', type=int, default=1, help='Repeat tests 1 to 5 times')
    args = parser.parse_args()
    if not 1 <= args.repeat <= 5:
        parser.error('--repeat must be between 1 and 5')
    stage = args.ghostty_stage.resolve(strict=True) if args.ghostty_stage else None
    if stage is not None:
        os.environ['TERMINAL_WORKSPACE_GHOSTTY_STAGE'] = str(stage)
    driver.prepare_build()
    with driver.exclusive_build_lock():
        if shutil.disk_usage('/private/tmp').free < 3 * GIB:
            raise RuntimeError('Less than 3 GiB free: synthetic window test not started')
        baseline = packages(ROOT / 'Cargo.lock')
        testing = packages(TEST_LOCK)
        if not lock_preserves_baseline(baseline, testing):
            raise RuntimeError('Synthetic window test lock is stale; update it against Cargo.lock')
        before, _ = driver.native_fingerprint()
        test_digest = hashlib.sha256(TEST_SOURCE.read_bytes()).hexdigest()
        lock_digest = hashlib.sha256(TEST_LOCK.read_bytes()).hexdigest()
        # Keep the fresh input snapshot separate from the modified test copy.
        # The temporary directory is owned by this invocation alone.
        with tempfile.TemporaryDirectory(prefix='twr-gpui-test-', dir='/private/tmp') as temp:
            scratch = Path(temp)
            cache = scratch / 'snapshots'
            with (scratch / 'snapshot.log').open('w') as log, contextlib.redirect_stdout(log):
                source_snapshot.snapshot(ROOT, cache, fresh=True)
            snapshot = Path(json.loads((cache / 'latest.json').read_text())['path'])
            driver.verify_fresh_source_times(snapshot, driver.BUILD / 'target')
            source = scratch / 'workspace'
            shutil.copytree(snapshot, source)
            manifest = source / 'crates/workspace-app/Cargo.toml'
            text = manifest.read_text()
            native = 'native = ["dep:gpui", "dep:objc"]'
            if text.count(native) != 1 or 'native-test =' in text:
                raise RuntimeError('Native feature layout changed; inspect the test helper')
            manifest.write_text(text.replace(native, native + '\nnative-test = ["native", "gpui/test-support"]', 1))
            shutil.copyfile(TEST_LOCK, source / 'Cargo.lock')
            with (source / 'crates/workspace-app/src/view.rs').open('a') as output:
                output.write('\n' + TEST_SOURCE.read_text())
            fake_home = scratch / 'home'
            fake_home.mkdir(mode=0o700)
            env = driver.environment()
            env.update({
                'HOME': str(fake_home),
                'XDG_CONFIG_HOME': str(fake_home / '.config'),
                'TMPDIR': '/private/tmp',
                'CARGO_NET_OFFLINE': 'true',
                'CARGO_BUILD_JOBS': '1',
                'CARGO_PROFILE_DEV_DEBUG': '0',
                'CARGO_INCREMENTAL': '0',
            })
            for name in ('TMUX', 'ENV', 'BASH_ENV', 'ZDOTDIR', 'RUSTC_WRAPPER',
                         'RUSTC_WORKSPACE_WRAPPER', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS',
                         'TERMINAL_WORKSPACE_TEST_TMUX_BINARY'):
                env.pop(name, None)
            features = 'native-test,ghostty' if stage else 'native-test'
            print('Running GPUI synthetic window tests; no native window or personal shell.', flush=True)
            for index in range(args.repeat):
                print(f'Run {index + 1}/{args.repeat}', flush=True)
                driver.guarded_run(
                    [str(driver.PINNED_BIN / 'cargo'), 'test', '--offline', '--locked',
                     '-p', 'workspace-app', '--features', features, '--bin', 'terminal-workspace',
                     'synthetic_window_probe'],
                    timeout=480, cwd=source, env=env,
                )
        after, _ = driver.native_fingerprint()
        if after != before or hashlib.sha256(TEST_SOURCE.read_bytes()).hexdigest() != test_digest \
                or hashlib.sha256(TEST_LOCK.read_bytes()).hexdigest() != lock_digest:
            raise RuntimeError('Source or test inputs changed during the run; result not accepted')
        print(json.dumps({'source_sha256': before, 'test_sha256': test_digest,
                          'test_lock_sha256': lock_digest,
                          'platform': 'GPUI synthetic; not AppKit or native IME',
                          'terminal_core': 'ghostty' if stage else 'fallback',
                          'runs': args.repeat}, indent=2))


if __name__ == '__main__':
    main()
