#!/usr/bin/env python3
"""Materialize only build inputs in a private, content-addressed directory."""
import hashlib
import json
import os
import shutil
from pathlib import Path
import stat
import sys
import tempfile


def reject_symlink_parents(root, path):
    current = root
    for part in path.relative_to(root).parts:
        current /= part
        if current.is_symlink():
            raise RuntimeError('Symlink in build inputs: ' + str(path.relative_to(root)))


def snapshot(root, cache, fresh=False):
    root = root.resolve(strict=True)
    cache.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = cache.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('Snapshot cache must be a private owned directory')
    # Only compile-time contracts and runtime/test resources. Reports and source
    # inspection fixtures are not build inputs and must not change this identity.
    paths = [root / name for name in (
        'Cargo.toml', 'Cargo.lock', 'Resources/Contracts/keymaps.json',
        'Resources/Contracts/agents-demo.json', 'Resources/Contracts/snacks-projects-config.json', 'Resources/Fixtures/native-tmux.conf',
        'Resources/Fixtures/tmux-control-output.json', 'Resources/Fixtures/nvim-case-line-fold.json', 'Resources/Fixtures/nvim-case-line-operator.json', 'Resources/Fixtures/nvim-case-line-repeat.json', 'Resources/Fixtures/nvim-case-line-after-failed.json', 'Resources/Fixtures/nvim-case-line-goto.json',
        'Resources/Fixtures/nvim-numbered-buffer-reference.json', 'Resources/Fixtures/nvim-buffer-saveas-reference.json', 'Resources/Fixtures/nvim-snacks-unlisted.json', 'Resources/Fixtures/nvim-unnamed-reopen-reference.json', 'Resources/Fixtures/nvim-unnamed-saveas-reference.json', 'Resources/Fixtures/nvim-bdelete-layout-reference.json', 'Resources/Fixtures/nvim-bdelete-target-reference.json', 'Resources/Fixtures/nvim-bdelete-terminal-reference.json', 'Resources/Fixtures/nvim-terminal-q-reference.json',
        'Resources/Fixtures/nvim-fold-indent-shift-reference.json',
        'Resources/Fixtures/nvim-paragraph-reference.json',
        'Resources/Fixtures/nvim-paragraph-operators.json',
        'Resources/Fixtures/nvim-paragraph-folds.json',
        'Resources/Fixtures/nvim-paragraph-visual-unicode.json',
        'Resources/Fixtures/nvim-paragraph-repeat.json',
        'Resources/Fixtures/nvim-jumplist-windows.json',
        'Resources/Fixtures/nvim-jumplist-search.json',
        'Resources/Fixtures/nvim-jumplist-buffers.json',
        'Resources/Fixtures/nvim-jumplist-unload.json',
        'Resources/Fixtures/nvim-jumplist-reload.json',
        'Resources/Fixtures/nvim-jumplist-fold-ui.json',
        'Resources/Fixtures/nvim-jumplist-terminal.json',
        'Resources/Fixtures/nvim-jumplist-two-term.json',
        'Resources/Fixtures/nvim-clearjumps.json',
        'Resources/Fixtures/nvim-wrap-control-scroll.json',
        'Resources/Fixtures/nvim-editor-panes-independent.json', 'Resources/Fixtures/nvim-editor-panes-independent.rs',
        'Resources/Fixtures/nvim-wrap-wheel-wide-unicode.json',
        'Resources/Fixtures/nvim-wrap-wheel-extra-geometries.json',
        'Resources/Fixtures/nvim-wrap-wheel-mixed-directions.json',
        'Resources/Fixtures/nvim-wrap-wheel-visual.json', 'Resources/Fixtures/nvim-wrap-wheel-visual-line.json', 'Resources/Fixtures/nvim-wrap-wheel-block-vspan.json', 'Resources/Fixtures/nvim-wrap-block-paint.json', 'Resources/Fixtures/nvim-wrap-block-eol-lazy-postkeys.json', 'Resources/Fixtures/nvim-wrap-block-lazy-postkeys.json', 'Resources/Fixtures/nvim-wrap-block-eol-paint.json', 'Resources/Fixtures/nvim-wrap-block-eol-wheel-paint.json', 'Resources/Fixtures/nvim-wrap-block-eol-wheel-intermediate.json', 'Resources/Fixtures/nvim-wrap-block-wheel-paint.json', 'Resources/Fixtures/nvim-wrap-wheel-block-extra-vspan.json', 'Resources/Fixtures/nvim-wrap-wheel-block-mixed-vspan.json',
        'Resources/Fixtures/nvim-wrap-wheel-insert.json',
        'Resources/Fixtures/nvim-wrap-wheel-matrix.json',
        'Resources/Fixtures/nvim-wrap-rows.jsonl', 'Resources/Fixtures/nvim-wrap-positions.jsonl', 'Resources/Fixtures/nvim-wrap-operators.jsonl', 'Resources/Fixtures/nvim-mouse-wheel.jsonl', 'Resources/Fixtures/nvim-line-operators.jsonl', 'Resources/Fixtures/nvim-percent.jsonl', 'Resources/Fixtures/nvim-end-count.jsonl', 'Resources/Fixtures/nvim-unicode-sequences.jsonl', 'Resources/Fixtures/nvim-unicode-word.jsonl', 'Resources/Fixtures/nvim-word-classes-0.12.1.txt', 'Resources/Fixtures/nvim-word-class-sequences.jsonl', 'Resources/Fixtures/nvim-word-objects-unicode.jsonl', 'Resources/Fixtures/nvim-word-objects-multiline.jsonl', 'Resources/Fixtures/nvim-multiline-change.jsonl', 'Resources/Fixtures/nvim-word-objects-change.jsonl', 'Resources/Fixtures/nvim-visual-word-objects.jsonl', 'Resources/Fixtures/nvim-visual-word-yank.jsonl', 'Resources/Fixtures/nvim-find-repeat.jsonl', 'Resources/Fixtures/nvim-find-operator-repeat.jsonl', 'Resources/Fixtures/nvim-textobject-replay.jsonl', 'Resources/Fixtures/nvim-word-multiline.jsonl', 'Resources/Fixtures/nvim-change-word.jsonl', 'Resources/Fixtures/nvim-line-fold-operators.jsonl', 'Resources/Fixtures/nvim-mouse-wheel-fold.jsonl', 'Resources/Fixtures/nvim-wrap-follow-limits.jsonl', 'Resources/Fixtures/nvim-wrap-short-matrix.json', 'Resources/Fixtures/nvim-wrap-visual-recenter-ui.json', 'Resources/Fixtures/nvim-wrap-recenter-geometries.json', 'Resources/Fixtures/nvim-wrap-recenter-unicode.json', 'Resources/Fixtures/nvim-wrap-bottom-redraw.json', 'Resources/Fixtures/nvim-wrap-mixed-matrix.json', 'Resources/Fixtures/nvim-wrap-fold-operators.jsonl', 'Resources/Fixtures/nvim-wrap-fold-change.jsonl', 'Resources/Fixtures/nvim-wrap-change.jsonl', 'Resources/Fixtures/nvim-pattern-reference.json', 'Resources/Fixtures/nvim-search-expansion.json', 'Resources/Fixtures/nvim-prompt-editing.json', 'Resources/Fixtures/nvim-prompt-caret.json', 'Resources/Fixtures/nvim-prompt-history.json', 'Resources/Fixtures/nvim-prompt-register.json', 'Resources/Fixtures/snacks-focus-reference.json', 'Resources/Fixtures/snacks-paging-reference.json', 'Resources/Fixtures/nvim-buffer-columns.json', 'Resources/Fixtures/nvim-buffer-views.json', 'Resources/Fixtures/nvim-editor-pane-focus.json', 'Resources/Fixtures/nvim-editor-pane-focus.rs', 'Resources/Fixtures/nvim-editor-panes.json', 'Resources/Fixtures/nvim-editor-panes.rs', 'Resources/Fixtures/nvim-list-extremes.json', 'Resources/Fixtures/nvim-window-count.json', 'Resources/Fixtures/nvim-window-focus.json', 'Resources/Fixtures/nvim-picker-input.json', 'Resources/Fixtures/nvim-visual-insert.json', 'Resources/Fixtures/synthetic-video.mov')]
    for name in ('crates', 'Resources/Demo', 'Resources/Helpers', 'Resources/Fonts', 'Resources/Notices', 'sample'):
        reject_symlink_parents(root, root / name)
        for base, directories, files in os.walk(root / name, followlinks=False):
            if any((Path(base) / d).is_symlink() for d in directories):
                raise RuntimeError('Symlink directory in build inputs')
            directories[:] = sorted(d for d in directories if d not in ('target', '__pycache__'))
            paths.extend(Path(base) / f for f in sorted(files) if not f.endswith('.pyc'))
    if len(paths) > 4096 or len(set(paths)) != len(paths):
        raise RuntimeError('Build snapshot has too many or duplicate inputs')
    directories = {parent for path in paths for parent in path.relative_to(root).parents if str(parent) != '.'}
    if (len(paths) + len(directories)) * 8192 + 32 * 1024**2 + 16 * 1024**2 > 64 * 1024**2:
        raise RuntimeError('Build snapshot metadata exceeds its managed reservation')
    stage = Path(tempfile.mkdtemp(prefix='pending-', dir=cache))
    manifest = {}
    total = 0
    digest = hashlib.sha256()
    for path in sorted(paths):
        rel = path.relative_to(root).as_posix()
        reject_symlink_parents(root, path)
        if path.is_symlink() or not path.is_file() or path.name.endswith('.env') or path.name.startswith('.env.'):
            raise RuntimeError('Unsupported build input: ' + rel)
        if path.stat().st_size > 8 * 1024 * 1024:
            raise RuntimeError('Build input exceeds 8 MiB: ' + rel)
        print('snapshot ' + rel, flush=True)
        data = path.read_bytes()
        total += len(data)
        if total > 32 * 1024 * 1024:
            raise RuntimeError('Build snapshot exceeds 32 MiB')
        sha = hashlib.sha256(data).hexdigest()
        digest.update(rel.encode() + b'\0' + bytes.fromhex(sha))
        manifest[rel] = sha
        dest = stage / rel
        dest.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        with dest.open('xb') as stream:
            stream.write(data)
        dest.chmod(0o600)
        if hashlib.sha256(dest.read_bytes()).hexdigest() != sha:
            raise RuntimeError('Snapshot readback mismatch: ' + rel)
    (stage / 'source-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    destination = cache / digest.hexdigest()
    # A new build needs a newly materialized source tree. Reusing an older
    # tree with a shared Cargo target can reuse binaries from a later snapshot.
    # Keep all snapshots immutable and retain failed stages for diagnosis.
    created_here = False
    if fresh:
        destination = cache / (digest.hexdigest() + '-' + stage.name.removeprefix('pending-'))
        if destination.exists() or destination.is_symlink():
            raise RuntimeError('Fresh snapshot destination already exists')
        stage.rename(destination)
        created_here = True
    elif destination.exists():
        for rel, sha in manifest.items():
            candidate = destination / rel
            if candidate.is_symlink() or hashlib.sha256(candidate.read_bytes()).hexdigest() != sha:
                raise RuntimeError('Existing snapshot was modified: ' + rel)
        # The duplicate is byte-for-byte verified and was created by this invocation.
        shutil.rmtree(stage)
    else:
        stage.rename(destination)
        created_here = True
    if created_here:
        # Lifecycle metadata lives outside the immutable source tree so it
        # does not alter the build fingerprint or archived source contents.
        markers = cache.parent / 'managed-snapshots'
        if markers.is_symlink():
            raise RuntimeError('Snapshot marker directory is a symlink')
        markers.mkdir(mode=0o700, exist_ok=True)
        info = markers.stat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise RuntimeError('Snapshot marker directory must be private and owned')
        marker = markers / (destination.name + '.json')
        fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'w') as out:
            json.dump({'schema': 1, 'snapshot': str(destination),
                       'manifest_sha256': hashlib.sha256((destination / 'source-manifest.json').read_bytes()).hexdigest()}, out)
            out.write('\n')
            out.flush()
            os.fsync(out.fileno())
    receipt = cache / 'latest.json'
    receipt.write_text(json.dumps({'path': str(destination), 'files': len(manifest), 'bytes': total}) + '\n')
    receipt.chmod(0o600)
    print('snapshot ready: ' + str(destination), flush=True)


if __name__ == '__main__':
    if len(sys.argv) not in (3, 4) or (len(sys.argv) == 4 and sys.argv[3] != '--fresh'):
        raise SystemExit('Usage: source_snapshot.py ROOT CACHE [--fresh]')
    snapshot(Path(sys.argv[1]), Path(sys.argv[2]), fresh=len(sys.argv) == 4)
