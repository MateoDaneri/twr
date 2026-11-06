#!/usr/bin/env python3
"""Install a verified normal release at a stable Mac Applications path.
Never starts the app, changes its profile, or removes a previous version.
"""
import argparse
import datetime
import hashlib
import importlib.util
import json
from pathlib import Path
import plistlib
import subprocess
import sys
import uuid

TARGET = Path('/Applications/Terminal Workspace Rust.app')
ROOT = Path(__file__).resolve().parents[1]
IDENTITY = 'dev.local.TerminalWorkspaceRust'
spec = importlib.util.spec_from_file_location('rust_workspace', ROOT / 'scripts/rust_workspace.py')
workspace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workspace)
retention = workspace.retention


def digest(path):
    with path.open('rb') as stream:
        result = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(chunk)
        return result.hexdigest()


def check_bundle(path, expected=None):
    if path.is_symlink() or not path.is_dir():
        raise RuntimeError('Expected a real app directory: ' + str(path))
    info = plistlib.loads((path / 'Contents/Info.plist').read_bytes())
    if info.get('CFBundleIdentifier') != IDENTITY:
        raise RuntimeError('Refusing a Development/Validation or unrelated app')
    subprocess.run(['/usr/bin/codesign', '--verify', '--strict', str(path)], check=True)
    actual = digest(path / 'Contents/MacOS/terminal-workspace')
    if expected and actual != expected:
        raise RuntimeError('Signed executable differs from the verified release receipt')
    return actual


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--receipt', type=Path, required=True)
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--confirmed-closed', action='store_true',
                        help='Required: confirm all app copies are closed before running')
    args = parser.parse_args()
    if not args.confirmed_closed:
        parser.error('Close all Terminal Workspace copies and pass --confirmed-closed')
    release = json.loads(args.receipt.read_text())
    if not args.dry_run:
        held = [Path(release['bundle'])]
        if release.get('archive'):
            held.append(Path(release['archive']))
        workspace.run_managed_retention(512 * 1024**2, protect=held)
    with retention.project_lock(ROOT):
        _, runtimes = retention.project_storage(ROOT)
        source = Path(release['bundle'])
        if not source.is_absolute() or '..' in source.parts or source.resolve(strict=True) != source:
            raise RuntimeError('Release bundle path must be absolute and contain no symlinks')
        matching = [runtime for runtime in runtimes if source.is_relative_to(runtime)]
        if len(matching) != 1:
            raise RuntimeError('Release bundle is not in a registered project runtime')
        with workspace.exclusive_build_lock(matching[0]):
            if not args.dry_run:
                paths = [source] + ([TARGET] if TARGET.exists() else [])
                size, entries = retention.bounded_logical_bytes(paths, max_total=384 * 1024**2,
                                                                 max_entries=16384)
                if TARGET.exists():
                    retention.bounded_logical_bytes([TARGET / 'Contents/Resources'], max_file=32 * 1024**2)
                reserve = size + entries * 8192 + 16 * 1024**2
                if reserve > 512 * 1024**2:
                    raise RuntimeError('Bundles exceed the managed installation reservation')
                workspace.managed_preflight('rollback' if TARGET.exists() else 'fixed_bundle', reserve)
            install_locked(args, release)
    if not args.dry_run:
        workspace.run_managed_retention()


def install_locked(args, release):
    if release.get('bundle_id') != IDENTITY or release.get('validation_instance') is not None or release.get('build_profile') != 'release':
        raise RuntimeError('Only a normal release receipt can be installed')
    source = Path(release['bundle'])
    expected = release['signed_binary_sha256']
    check_bundle(source, expected)
    archive_path = release.get('archive')
    archive_hash = release.get('archive_sha256')
    if (archive_path is None) != (archive_hash is None):
        raise RuntimeError('Release receipt has incomplete archive information')
    if archive_path is not None:
        archive = Path(archive_path)
        if archive.is_symlink() or not archive.is_file() or digest(archive) != archive_hash:
            raise RuntimeError('Archive differs from the verified release receipt')
    previous_sha = None
    if TARGET.exists() or TARGET.is_symlink():
        previous_sha = check_bundle(TARGET)
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    token = uuid.uuid4().hex[:8]
    backup = ROOT / 'dist/installed-backups' / (stamp + '-' + token) / TARGET.name
    stage = TARGET.parent / ('.Terminal Workspace Rust.install-' + token + '.app')
    result = {'target': str(TARGET), 'source': str(source), 'source_sha256': release['source_sha256'],
              'signed_binary_sha256': expected, 'backup': str(backup) if TARGET.exists() else None,
              'profile_modified': False, 'launched': False, 'closed_precondition_confirmed': True}
    if args.dry_run:
        print(json.dumps({'dry_run': True, **result}, indent=2))
        return
    if stage.exists() or stage.is_symlink():
        raise RuntimeError('Unexpected staging collision; preserved')
    subprocess.run(['/usr/bin/ditto', str(source), str(stage)], check=True)
    check_bundle(stage, expected)
    previous = TARGET.exists()
    if previous:
        backup.parent.mkdir(parents=True, exist_ok=False)
        TARGET.rename(backup)
    try:
        stage.rename(TARGET)
        check_bundle(TARGET, expected)
    except Exception:
        # Preserve even an unexpected candidate; restore the old app if present.
        if TARGET.exists() and not stage.exists():
            TARGET.rename(stage)
        if previous and backup.exists() and not TARGET.exists():
            backup.rename(TARGET)
        raise
    if previous:
        # Older backups have no marker and stay protected. Only new, fully
        # installed rollbacks may participate in the reviewed lifecycle.
        marker = backup.parent / '.twr-rollback.json'
        with marker.open('x') as output:
            json.dump({'schema': 1, 'retention_managed': True, 'bundle': TARGET.name,
                       'signed_binary_sha256': previous_sha}, output)
            output.write('\n')
        marker.chmod(0o600)
    receipt = ROOT / 'dist' / ('installed-' + stamp + '-' + token + '.json')
    receipt.write_text(json.dumps(result, indent=2) + '\n')
    try:
        retention.finish_managed_stage(ROOT, source, expected_source=release['source_sha256'],
                                       expected_binary=expected)
    except (OSError, RuntimeError, ValueError) as error:
        print('Installed, but candidate remains protected: ' + str(error), file=sys.stderr)
    print(json.dumps({**result, 'installation_receipt': str(receipt)}, indent=2))


if __name__ == '__main__':
    main()
