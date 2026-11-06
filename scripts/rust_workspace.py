#!/usr/bin/env python3
"""Project-local Rust toolchain and bounded checks. Never reads user dotfiles."""
import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import time
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
TOOLS = (ROOT / '.toolchain').resolve()
RUST_VERSION = '1.99.1'
CACHE_TAG = b'Signature: 8a477f597d28d172789f06886806bc55\n# Terminal Workspace generated Cargo artifacts.\n'
retention_spec = importlib.util.spec_from_file_location('artifact_retention', ROOT / 'scripts/artifact_retention.py')
retention = importlib.util.module_from_spec(retention_spec)
retention_spec.loader.exec_module(retention)


def build_directory():
    default = Path('/private/tmp') / ('terminal-workspace-rust-' + str(os.getuid()) + '-' + hashlib.sha256(str(ROOT).encode()).hexdigest()[:12])
    marker = ROOT / '.runtime-location'
    if not marker.exists() and not marker.is_symlink():
        return default
    info = marker.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077 or info.st_size > 1024:
        raise RuntimeError('Runtime location must be a private, owned regular file')
    candidate = Path(marker.read_text().strip())
    if candidate.parent != Path('/private/tmp') or not re.fullmatch(r'terminal-workspace-rust-' + str(os.getuid()) + r'-[0-9a-f]{12}', candidate.name):
        raise RuntimeError('Runtime location must name this user\'s private temporary build cache')
    if candidate.exists() and (candidate.is_symlink() or not candidate.is_dir() or candidate.stat().st_uid != os.getuid()):
        raise RuntimeError('Runtime cache is not an owned directory')
    return candidate


BUILD = build_directory()
PINNED_BIN = BUILD / 'toolchains' / (RUST_VERSION + '-aarch64-apple-darwin') / 'bin'


def native_fingerprint(root=ROOT):
    files = sorted([*root.glob('crates/*/src/*.rs'), *root.glob('crates/*/Cargo.toml'), root / 'Cargo.toml', root / 'Cargo.lock', root / 'Resources/Contracts/keymaps.json', root / 'Resources/Contracts/agents-demo.json', root / 'Resources/Contracts/snacks-projects-config.json', root / 'Resources/Demo/projects/snacks-projects/discovered-demo/compose.yaml', root / 'Resources/Fixtures/tmux-control-output.json', root / 'Resources/Fixtures/nvim-case-line-fold.json', root / 'Resources/Fixtures/nvim-case-line-operator.json', root / 'Resources/Fixtures/nvim-case-line-repeat.json', root / 'Resources/Fixtures/nvim-case-line-after-failed.json', root / 'Resources/Fixtures/nvim-case-line-goto.json', root / 'Resources/Fixtures/nvim-wrap-wheel-wide-unicode.json', root / 'Resources/Fixtures/nvim-wrap-wheel-extra-geometries.json', root / 'Resources/Fixtures/nvim-wrap-wheel-mixed-directions.json', root / 'Resources/Fixtures/nvim-wrap-wheel-visual.json', root / 'Resources/Fixtures/nvim-wrap-wheel-visual-line.json', root / 'Resources/Fixtures/nvim-wrap-wheel-block-vspan.json', root / 'Resources/Fixtures/nvim-wrap-block-paint.json', root / 'Resources/Fixtures/nvim-wrap-block-eol-lazy-postkeys.json', root / 'Resources/Fixtures/nvim-wrap-block-lazy-postkeys.json', root / 'Resources/Fixtures/nvim-wrap-block-eol-paint.json', root / 'Resources/Fixtures/nvim-wrap-block-eol-wheel-paint.json', root / 'Resources/Fixtures/nvim-wrap-block-eol-wheel-intermediate.json', root / 'Resources/Fixtures/nvim-wrap-block-wheel-paint.json', root / 'Resources/Fixtures/nvim-wrap-wheel-block-extra-vspan.json', root / 'Resources/Fixtures/nvim-wrap-wheel-block-mixed-vspan.json', root / 'Resources/Fixtures/nvim-wrap-wheel-insert.json', root / 'Resources/Fixtures/nvim-wrap-wheel-matrix.json', root / 'Resources/Fixtures/nvim-wrap-rows.jsonl', root / 'Resources/Fixtures/nvim-wrap-positions.jsonl', root / 'Resources/Fixtures/nvim-wrap-operators.jsonl', root / 'Resources/Fixtures/nvim-mouse-wheel.jsonl', root / 'Resources/Fixtures/nvim-line-operators.jsonl', root / 'Resources/Fixtures/nvim-percent.jsonl', root / 'Resources/Fixtures/nvim-end-count.jsonl', root / 'Resources/Fixtures/nvim-unicode-sequences.jsonl', root / 'Resources/Fixtures/nvim-unicode-word.jsonl', root / 'Resources/Fixtures/nvim-word-classes-0.12.1.txt', root / 'Resources/Fixtures/nvim-word-class-sequences.jsonl', root / 'Resources/Fixtures/nvim-word-objects-unicode.jsonl', root / 'Resources/Fixtures/nvim-word-objects-multiline.jsonl', root / 'Resources/Fixtures/nvim-multiline-change.jsonl', root / 'Resources/Fixtures/nvim-word-objects-change.jsonl', root / 'Resources/Fixtures/nvim-visual-word-objects.jsonl', root / 'Resources/Fixtures/nvim-visual-word-yank.jsonl', root / 'Resources/Fixtures/nvim-find-repeat.jsonl', root / 'Resources/Fixtures/nvim-find-operator-repeat.jsonl', root / 'Resources/Fixtures/nvim-textobject-replay.jsonl', root / 'Resources/Fixtures/nvim-word-multiline.jsonl', root / 'Resources/Fixtures/nvim-change-word.jsonl', root / 'Resources/Fixtures/nvim-line-fold-operators.jsonl', root / 'Resources/Fixtures/nvim-mouse-wheel-fold.jsonl', root / 'Resources/Fixtures/nvim-wrap-follow-limits.jsonl', root / 'Resources/Fixtures/nvim-wrap-short-matrix.json', root / 'Resources/Fixtures/nvim-wrap-visual-recenter-ui.json', root / 'Resources/Fixtures/nvim-wrap-recenter-geometries.json', root / 'Resources/Fixtures/nvim-wrap-recenter-unicode.json', root / 'Resources/Fixtures/nvim-wrap-bottom-redraw.json', root / 'Resources/Fixtures/nvim-wrap-mixed-matrix.json', root / 'Resources/Fixtures/nvim-wrap-fold-operators.jsonl', root / 'Resources/Fixtures/nvim-wrap-fold-change.jsonl', root / 'Resources/Fixtures/nvim-wrap-change.jsonl', root / 'Resources/Fixtures/nvim-pattern-reference.json', root / 'Resources/Fixtures/nvim-search-expansion.json', root / 'Resources/Fixtures/nvim-prompt-editing.json', root / 'Resources/Fixtures/nvim-prompt-caret.json', root / 'Resources/Fixtures/nvim-prompt-history.json', root / 'Resources/Fixtures/nvim-prompt-register.json', root / 'Resources/Fixtures/snacks-focus-reference.json', root / 'Resources/Fixtures/snacks-paging-reference.json', root / 'Resources/Fixtures/nvim-buffer-columns.json', root / 'Resources/Fixtures/nvim-buffer-views.json', root / 'Resources/Fixtures/nvim-editor-pane-focus.json', root / 'Resources/Fixtures/nvim-editor-pane-focus.rs', root / 'Resources/Fixtures/nvim-editor-panes.json', root / 'Resources/Fixtures/nvim-editor-panes.rs', root / 'Resources/Fixtures/nvim-list-extremes.json', root / 'Resources/Fixtures/nvim-window-count.json', root / 'Resources/Fixtures/nvim-window-focus.json', root / 'Resources/Fixtures/nvim-picker-input.json', root / 'Resources/Fixtures/nvim-visual-insert.json', root / 'Resources/Fixtures/native-tmux.conf', root / 'Resources/Fonts/JetBrainsMonoNerdFont-Regular.ttf'])
    files = sorted([*files, *root.glob('crates/*/build.rs'), *root.glob('crates/*/native/*.c'), *root.glob('crates/*/native/*.h'), *root.glob('Resources/Notices/*'), *root.glob('Resources/Demo/projects/workspace/external-open/*'), *root.glob('Resources/Helpers/*.py'), *root.glob('Resources/Helpers/*.m'), root / 'Resources/Fixtures/nvim-numbered-buffer-reference.json', root / 'Resources/Fixtures/nvim-snacks-unlisted.json', root / 'Resources/Fixtures/nvim-buffer-saveas-reference.json', root / 'Resources/Fixtures/nvim-unnamed-reopen-reference.json', root / 'Resources/Fixtures/nvim-unnamed-saveas-reference.json', root / 'Resources/Fixtures/nvim-bdelete-layout-reference.json', root / 'Resources/Fixtures/nvim-bdelete-target-reference.json', root / 'Resources/Fixtures/nvim-bdelete-terminal-reference.json', root / 'Resources/Fixtures/synthetic-video.mov', root / 'Resources/Demo/projects/workspace/docs/synthetic-video.mov', root / 'Resources/Demo/projects/workspace/docs/preview.html'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-terminal-q-reference.json', root / 'Resources/Fixtures/nvim-fold-indent-shift-reference.json', root / 'Resources/Fixtures/nvim-paragraph-reference.json', root / 'Resources/Fixtures/nvim-paragraph-operators.json', root / 'Resources/Fixtures/nvim-editor-panes-independent.json', root / 'Resources/Fixtures/nvim-editor-panes-independent.rs'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-paragraph-folds.json'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-paragraph-visual-unicode.json'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-paragraph-repeat.json'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-jumplist-windows.json'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-jumplist-search.json'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-jumplist-buffers.json'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-jumplist-unload.json'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-jumplist-reload.json'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-jumplist-fold-ui.json'])
    files = sorted([*files, root / 'Resources/Fixtures/nvim-jumplist-terminal.json', root / 'Resources/Fixtures/nvim-jumplist-two-term.json', root / 'Resources/Fixtures/nvim-clearjumps.json', root / 'Resources/Fixtures/nvim-wrap-control-scroll.json'])
    digest = hashlib.sha256()
    for path in files:
        digest.update(str(path.relative_to(root)).encode() + b'\0' + path.read_bytes() + b'\0')
    return digest.hexdigest(), [str(path.relative_to(root)) for path in files]


def environment():
    env = os.environ.copy()
    env['CARGO_HOME'] = str(TOOLS / 'cargo')
    env['RUSTUP_HOME'] = str(TOOLS / 'rustup')
    env['CARGO_TARGET_DIR'] = str(BUILD / 'target')
    env.setdefault('CARGO_BUILD_JOBS', '2')
    # Repeated native iterations accumulated several GiB of intermediate objects.
    env.setdefault('CARGO_INCREMENTAL', '0')
    paths = [str(TOOLS / 'cargo/bin')]
    if PINNED_BIN.is_dir():
        paths.insert(0, str(PINNED_BIN))
        env['RUSTC'] = str(PINNED_BIN / 'rustc')
        env['RUSTDOC'] = str(PINNED_BIN / 'rustdoc')
    env['PATH'] = os.pathsep.join(paths) + os.pathsep + env.get('PATH', '')
    return env


def enable_ghostty(flags):
    if '--features' in flags:
        flags[flags.index('--features') + 1] += ',ghostty'
    else:
        # Cargo flags must precede the rustc/clippy/program argument separator.
        position = flags.index('--') if '--' in flags else len(flags)
        flags[position:position] = ['--features', 'workspace-app/ghostty']


def prepare_build():
    if BUILD.is_symlink():
        raise RuntimeError('Build directory must not be a symlink.')
    BUILD.mkdir(mode=0o700, exist_ok=True)
    if BUILD.stat().st_uid != os.getuid():
        raise RuntimeError('Build directory belongs to another user.')
    BUILD.chmod(0o700)
    target = BUILD / 'target'
    if target.is_symlink():
        raise RuntimeError('Cargo target must not be a symlink.')
    target.mkdir(exist_ok=True)
    if target.stat().st_uid != os.getuid():
        raise RuntimeError('Cargo target belongs to another user.')
    tag = target / 'CACHEDIR.TAG'
    if tag.exists() or tag.is_symlink():
        info = tag.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > 1024 or not tag.read_bytes().startswith(CACHE_TAG.split(b'\n', 1)[0]):
            raise RuntimeError('Cargo cache tag is invalid; existing file preserved.')
    else:
        with tag.open('xb') as stream:
            stream.write(CACHE_TAG)


def run_managed_retention(reserve_bytes=0, protect=()):
    """Run bounded post-operation maintenance after releasing build locks."""
    try:
        spec = importlib.util.spec_from_file_location('artifact_maintenance', ROOT / 'scripts/artifact_maintenance.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        module.reconcile(reserve_bytes=reserve_bytes, protect=protect)
    except Exception as error:
        print('Retention incomplete; artifacts preserved or require review: ' + str(error), file=sys.stderr)


def managed_preflight(kind, reserve_bytes, *, archive=False):
    """Called with the project lock before creating managed artifacts."""
    spec = importlib.util.spec_from_file_location('artifact_inventory', ROOT / 'scripts/artifact_inventory.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.preflight(ROOT, kind=kind, reserve_bytes=reserve_bytes, archive=archive)


def verify_fresh_source_times(source, target):
    # Cargo freshness depends on mtimes, even for distinct source directories.
    # Fail closed if a clock rollback or an external build made workspace
    # artifacts at least as new as any freshly copied input.
    manifest = json.loads((source / 'source-manifest.json').read_text())
    if not isinstance(manifest, dict) or not manifest:
        raise RuntimeError('Snapshot manifest is empty or invalid')
    source_times = []
    for name in manifest:
        relative = Path(name)
        if relative.is_absolute() or '..' in relative.parts:
            raise RuntimeError('Invalid path in snapshot manifest')
        path = source / relative
        if path.is_symlink() or not path.is_file():
            raise RuntimeError('Snapshot input is missing or not a regular file')
        source_times.append(path.stat().st_mtime_ns)
    artifact_times = []
    for profile in ('debug', 'release'):
        base = target / profile
        candidates = list((base / '.fingerprint').glob('workspace-*/invoked.timestamp'))
        for pattern in ('workspace_app*', 'libworkspace_app*', 'workspace_core*',
                        'libworkspace_core*', 'workspace_cli*', 'libworkspace_cli*'):
            candidates.extend((base / 'deps').glob(pattern))
        candidates.extend([base / 'terminal-workspace', base / 'workspace-cli'])
        for path in candidates:
            if path.is_file():
                artifact_times.append(path.stat().st_mtime_ns)
    if artifact_times and min(source_times) <= max(artifact_times):
        raise RuntimeError('Build stopped: source timestamps are not newer than shared workspace artifacts')


@contextmanager
def exclusive_build_lock(build=BUILD):
    # One owner for snapshot selection, Cargo output and build receipt.
    path = build / 'driver.lock'
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise RuntimeError('Build lock must be a private owned regular file')
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise RuntimeError('Another build driver is active; no work started') from exc
        yield
    finally:
        os.close(fd)


def guarded_run(argv, *, timeout, cwd, env, minimum_free=2*1024**3):
    process = subprocess.Popen(argv, cwd=cwd, env=env, start_new_session=True)
    started = time.monotonic()
    try:
        while process.poll() is None:
            if shutil.disk_usage('/private/tmp').free < minimum_free:
                raise RuntimeError('Release stopped: less than 2 GiB free; partial build artifacts preserved.')
            if time.monotonic() - started > timeout:
                raise RuntimeError('Release build exceeded its time limit; artifacts preserved.')
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
        if process.returncode:
            raise subprocess.CalledProcessError(process.returncode, argv)
    except BaseException:
        try:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=5)
        except ProcessLookupError:
            pass
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        raise


def run(argv, timeout=180, cwd=ROOT):
    prepare_build()
    if argv and argv[0] == str(TOOLS / 'cargo/bin/cargo') and (PINNED_BIN / 'cargo').is_file():
        argv = [str(PINNED_BIN / 'cargo'), *argv[1:]]
    return subprocess.run(argv, cwd=cwd, env=environment(), timeout=timeout, check=True)


def bootstrap():
    # Executable toolchains must be materialized outside Documents/iCloud.
    # The installer preserves the old .toolchain tree and verifies Rust hashes.
    return subprocess.run([sys.executable, str(ROOT / 'scripts/repair_rust_toolchain.py')],
                          cwd=ROOT, timeout=480, check=True)


def doctor():
    cargo = PINNED_BIN / 'cargo'
    metal = subprocess.run(['/usr/bin/xcrun', '-f', 'metal'], capture_output=True, text=True)
    print(json.dumps({'host': platform.system() + ' ' + platform.machine(),
                      'localCargo': cargo.exists(), 'metalCompiler': metal.returncode == 0,
                      'buildCache': str(BUILD),
                      'gui': 'GPUI 0.2.2 native app; runtime_shaders enabled, no external metal compiler required for this profile',
                      'configurationInspection': 'not_requested'}, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['bootstrap', 'doctor', 'test', 'check', 'demo', 'pty-smoke', 'bench', 'edit-bench', 'fmt', 'format', 'resolve', 'gui-build', 'gui-release-build', 'gui-check', 'gui'])
    parser.add_argument('--ghostty-stage', type=Path, help='Opt into the evaluated C core using this prepared private stage')
    args = parser.parse_args()
    if args.ghostty_stage:
        os.environ['TERMINAL_WORKSPACE_GHOSTTY_STAGE'] = str(args.ghostty_stage.resolve(strict=True))
    if args.command == 'doctor':
        doctor()
        return
    # Validate before the snapshot helper or installer can create children.
    prepare_build()
    if args.command not in ('bootstrap', 'format', 'fmt', 'resolve'):
        run_managed_retention(64 * 1024**2)
    with retention.project_lock(ROOT):
        with exclusive_build_lock():
            if args.command not in ('bootstrap', 'format', 'fmt', 'resolve'):
                managed_preflight('snapshot', 64 * 1024**2)
            main_locked(args)
    if args.command not in ('bootstrap', 'format', 'fmt', 'resolve'):
        run_managed_retention()


def main_locked(args):
    if args.command == 'bootstrap':
        bootstrap()
        return
    if args.command not in ('format', 'fmt', 'resolve') and shutil.disk_usage('/private/tmp').free < 2 * 1024**3:
        raise RuntimeError('Less than 2 GiB free: build stopped before allocating artifacts. Free space first; no automatic deletion is performed.')
    cargo = PINNED_BIN / 'cargo'
    if not cargo.exists():
        raise RuntimeError('Run python3 scripts/rust_workspace.py bootstrap first; the private runtime toolchain may have been removed by temporary-file cleanup.')
    options = {
        'resolve': ['generate-lockfile'],
        'gui-release-build': ['build', '--release', '--locked', '-p', 'workspace-app', '--features', 'native'],
        'gui-build': ['build', '--locked', '-p', 'workspace-app', '--features', 'native'],
        'gui-check': ['clippy', '--locked', '-p', 'workspace-app', '--features', 'native', '--all-targets', '--', '-D', 'warnings'],
        'gui': ['run', '--locked', '-p', 'workspace-app', '--features', 'native', '--bin', 'terminal-workspace'],
        'test': ['test', '--workspace', '--locked'],
        'check': ['clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings'],
        'fmt': ['fmt', '--all', '--', '--check'],
        'format': ['fmt', '--all'],
        'demo': ['run', '--locked', '-p', 'workspace-cli', '--', 'demo'],
        'pty-smoke': ['run', '--locked', '-p', 'workspace-cli', '--', 'pty-smoke'],
        'edit-bench': ['run', '--release', '--locked', '-p', 'workspace-cli', '--', 'edit-bench'],
        'bench': ['run', '--release', '--locked', '-p', 'workspace-cli', '--', 'bench'],
    }
    if args.ghostty_stage and args.command not in ('format', 'fmt', 'resolve'):
        flags = options[args.command]
        enable_ghostty(flags)
    source_root = ROOT
    if args.command not in ('format', 'fmt', 'resolve'):
        # Reading a dataless checkout during rustc caused intermittent OS errors.
        # Copy only explicit build inputs first, then compile the verified snapshot.
        cache = BUILD / 'source-snapshots'
        retention.capacity(ROOT, kind='source-snapshots', count=1600, budget=12 * 1024**3,
                           reserve=64 * 1024**2, label='Source snapshots', include=lambda name: name != 'latest.json')
        # Never compile from a previously used source tree: shared Cargo output
        # can otherwise be newer than its old mtimes and belong to another tree.
        subprocess.run([sys.executable, str(ROOT / 'scripts/source_snapshot.py'), str(ROOT), str(cache), '--fresh'],
                       check=True, timeout=180)
        source_root = Path(json.loads((cache / 'latest.json').read_text())['path'])
        verify_fresh_source_times(source_root, BUILD / 'target')
    if args.command == 'gui-release-build':
        guarded_run([str(cargo)] + options[args.command], timeout=480, cwd=source_root, env=environment())
    else:
        run([str(cargo)] + options[args.command], timeout=None if args.command == 'gui' else 480, cwd=source_root)
    if args.command in ('gui-build', 'gui-release-build'):
        source_sha, files = native_fingerprint(source_root)
        release = args.command == 'gui-release-build'
        profile = 'release' if release else 'debug'
        binary = BUILD / 'target' / profile / 'terminal-workspace'
        receipt = BUILD / ('build-receipt-release.json' if release else 'build-receipt.json')
        core = None
        if args.ghostty_stage:
            stage = args.ghostty_stage.resolve(strict=True)
            prepared = json.loads((stage / 'receipt.json').read_text())
            compiled = json.loads(sorted(stage.glob('build-*.json'))[-1].read_text())
            archive = stage / 'install/lib/libghostty-vt.a'
            expected = next(a['sha256'] for a in compiled['artifacts'] if a['path'] == str(archive))
            if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
                raise RuntimeError('Terminal core archive changed during build')
            core = {'commit': prepared['commit'], 'static_archive_sha256': expected, 'graphics': 'experimental Kitty RGB/RGBA and bounded PNG in GPUI; animation pending; file/temp/shm disabled'}
        receipt.write_text(json.dumps({'profile': profile, 'source_sha256': source_sha, 'source_files': files, 'source_snapshot': str(source_root),
                                       'terminal_engine': 'libghostty-vt' if args.ghostty_stage else 'vt100', 'terminal_core': core,
                                       'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}, indent=2) + '\n')
        receipt.chmod(0o600)


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print('ERROR: ' + str(error), file=sys.stderr)
        sys.exit(1)
