#!/usr/bin/env python3
"""Produce a read-only, exact-path proposal; never execute or authorize it."""
import importlib.util
import json
from pathlib import Path
import re
import os
import stat
import sys

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('artifact_inventory', ROOT / 'scripts/artifact_inventory.py')
inventory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inventory)
retention = inventory.retention


def unresolved_rollback_paths():
    """Protect a rollback until each recorded retirement has completed."""
    journal = retention.common_git_dir(ROOT) / 'retention-actions.jsonl'
    if not journal.exists() and not journal.is_symlink():
        return set()
    fd = os.open(journal, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as source:
        info = os.fstat(source.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_mode & 0o077 or info.st_size > 5 * 1024**2):
            raise RuntimeError('Retention journal must be a bounded private owned file')
        raw = source.read(5 * 1024**2 + 1)
    if len(raw) > 5 * 1024**2 or (raw and not raw.endswith(b'\n')):
        raise RuntimeError('Incomplete retention journal; preserve artifacts for review')
    pending = set()
    for line in raw.splitlines():
        try:
            event = json.loads(line)
        except (ValueError, UnicodeDecodeError) as error:
            raise RuntimeError('Invalid retention journal; preserve artifacts for review') from error
        if not isinstance(event, dict):
            raise RuntimeError('Invalid retention event; preserve artifacts for review')
        if event.get('kind') != 'rollback':
            continue
        action, path, quarantine = (event.get(k) for k in ('action', 'path', 'quarantine'))
        if (action not in ('intent', 'removed', 'removal_incomplete')
                or not isinstance(path, str) or not Path(path).is_absolute()
                or not isinstance(quarantine, str) or not Path(quarantine).is_absolute()):
            raise RuntimeError('Invalid rollback journal event; preserve artifacts for review')
        key = (path, quarantine)
        if action == 'intent':
            pending.add(key)
        elif action == 'removed':
            pending.discard(key)
        else:
            pending.add(key)
    return {Path(path) for path, _ in pending}


def stage_proposal(path, keep):
    reasons = []
    if path.is_symlink() or not path.is_dir():
        reasons.append('not a real stage directory')
    if path in keep:
        reasons.append('among two newest stages in this runtime')
    marker = path / '.twr-retention.json'
    data = None
    if marker.is_symlink() or not marker.is_file():
        reasons.append('no recognized provenance marker')
    else:
        try:
            data = json.loads(marker.read_text())
        except (OSError, ValueError):
            reasons.append('invalid provenance marker')
    if data is not None:
        if data.get('schema') not in (1, 2) or data.get('kind') not in ('development', 'validation', 'release'):
            reasons.append('unrecognized marker schema or kind')
        if data.get('schema') != 2 or data.get('retention_managed') is not True:
            reasons.append('legacy artifact; explicit migration classification required')
        if data.get('in_flight') is not False:
            reasons.append('candidate remains in flight')
        if not re.fullmatch(r'[0-9a-f]{64}', str(data.get('source_sha256', ''))):
            reasons.append('missing source fingerprint')
        if not re.fullmatch(r'[0-9a-f]{64}', str(data.get('binary_sha256', ''))):
            reasons.append('missing signed executable hash')
        source = Path(data.get('source_snapshot', ''))
        if (source.parent != path.parent.parent / 'source-snapshots'
                or source.is_symlink() or not source.is_dir()):
            reasons.append('source snapshot unavailable or outside this runtime')
        if data.get('archive'):
            if not isinstance(data['archive'], str):
                reasons.append('invalid paired archive')
            else:
                archive = Path(data['archive'])
                if archive.exists() or archive.is_symlink():
                    reasons.append('paired archive remains available')
    if retention.explicitly_pinned(ROOT, path):
        reasons.append('explicitly pinned')
    # None are marked safe by a read-only listing. At action time the operator
    # must revalidate ownership, code signature, all source and resource hashes,
    # archive readback, references across worktrees and process activity while
    # holding the common and runtime locks. A stale plan must be rejected.
    info = path.lstat()
    return {'path': str(path), 'device': info.st_dev, 'inode': info.st_ino,
            'marker_sha256': retention.file_digest(marker).hex() if data is not None else None,
            'classification': 'protected' if reasons else 'candidate for further review',
            'reasons': reasons, 'authorized_for_removal': False,
            'remaining_checks': ['exact directory and inode identity', 'source and resource provenance',
                                 'verified compact source archive', 'signed binary and manifest',
                                 'explicit pins and current receipts', 'no open or running process',
                                 'reviewed exact-path authorization immediately before removal']}


def snapshot_proposal(path, keep):
    reasons = []
    build = path.parent.parent
    if path.is_symlink() or not path.is_dir():
        reasons.append('not a real snapshot directory')
    if path in keep:
        reasons.append('among two newest snapshots in this runtime')
    if not re.fullmatch(r'[0-9a-f]{64}(?:-[A-Za-z0-9_-]+)?', path.name):
        reasons.append('not a completed snapshot name')
    manifest = path / 'source-manifest.json'
    if manifest.is_symlink() or not manifest.is_file():
        reasons.append('no source manifest')
    # Legacy snapshots have no managed marker and require separate migration.
    marker = build / 'managed-snapshots' / (path.name + '.json')
    if marker.parent.is_symlink() or marker.is_symlink() or not marker.is_file():
        reasons.append('legacy snapshot; explicit migration classification required')
    else:
        marker_info = marker.lstat()
        if (not stat.S_ISREG(marker_info.st_mode) or marker_info.st_uid != os.getuid()
                or marker_info.st_mode & 0o077 or marker_info.st_size > 4096):
            reasons.append('invalid or non-private snapshot marker')
        else:
            try:
                metadata = json.loads(marker.read_text())
                if (metadata.get('schema') != 1 or metadata.get('snapshot') != str(path)
                        or not manifest.is_file()
                        or metadata.get('manifest_sha256') != retention.file_digest(manifest).hex()):
                    reasons.append('snapshot marker does not match its manifest')
            except (OSError, ValueError):
                reasons.append('invalid snapshot marker')
    if retention.active_source(build, path):
        reasons.append('referenced by a current receipt or stage')
    if retention.explicitly_pinned(ROOT, path):
        reasons.append('explicitly pinned')
    info = path.lstat()
    return {'path': str(path), 'device': info.st_dev, 'inode': info.st_ino,
            'manifest_sha256': retention.file_digest(manifest).hex() if manifest.is_file() and not manifest.is_symlink() else None,
            'marker_sha256': retention.file_digest(marker).hex() if marker.is_file() and not marker.is_symlink() else None,
            'classification': 'protected' if reasons else 'candidate for further review',
            'reasons': reasons, 'authorized_for_removal': False,
            'remaining_checks': ['exact directory and inode identity', 'source manifest and resource hashes',
                                 'verified compact source archive', 'explicit pins and current receipts',
                                 'no open or running process', 'reviewed exact-path authorization immediately before removal']}


def rollback_proposal(path, keep):
    reasons = []
    if path.is_symlink() or not path.is_dir():
        reasons.append('not a real backup directory')
    if path in keep:
        reasons.append('newest rollback across registered worktrees')
    if path in unresolved_rollback_paths():
        reasons.append('unfinished retirement recorded; preserve for review')
    marker = path / '.twr-rollback.json'
    if marker.is_symlink() or not marker.is_file():
        reasons.append('legacy rollback; separately protected')
    else:
        try:
            data = json.loads(marker.read_text())
            info = marker.lstat()
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                    or info.st_mode & 0o077 or data.get('schema') != 1
                    or data.get('retention_managed') is not True
                    or data.get('bundle') != 'Terminal Workspace Rust.app'
                    or not re.fullmatch(r'[0-9a-f]{64}', str(data.get('signed_binary_sha256', '')))):
                reasons.append('invalid managed rollback marker')
        except (OSError, ValueError):
            reasons.append('invalid managed rollback marker')
    if retention.explicitly_pinned(ROOT, path):
        reasons.append('explicitly pinned')
    info = path.lstat()
    return {'path': str(path), 'device': info.st_dev, 'inode': info.st_ino,
            'marker_sha256': retention.file_digest(marker).hex() if marker.is_file() and not marker.is_symlink() else None,
            'classification': 'protected' if reasons else 'candidate for further review',
            'reasons': reasons, 'authorized_for_removal': False,
            'remaining_checks': ['verified compact source and resources', 'signature and installed app',
                                 'no open or running process', 'explicit pins and newest rollback']}


def archive_proposal(path, keep):
    reasons = []
    if path.is_symlink() or not path.is_file():
        reasons.append('not a real archive')
    if path in keep:
        reasons.append('among two newest archives')
    marker = path.with_name(path.name + '.twr-archive.json')
    if marker.is_symlink() or not marker.is_file():
        reasons.append('legacy or unclassified archive; separately protected')
    else:
        try:
            info = marker.lstat()
            data = json.loads(marker.read_text())
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                    or info.st_mode & 0o077 or data.get('schema') != 1
                    or data.get('retention_managed') is not True or data.get('archive') != str(path)
                    or not re.fullmatch(r'[0-9a-f]{64}', str(data.get('archive_sha256', '')))
                    or not re.fullmatch(r'[0-9a-f]{64}', str(data.get('source_sha256', '')))):
                reasons.append('invalid managed archive marker')
            if data.get('in_flight') is not False:
                reasons.append('archive remains in flight')
            bundle = Path(data.get('bundle', ''))
            if bundle.is_symlink() or not bundle.is_dir():
                reasons.append('paired bundle unavailable')
        except (OSError, ValueError, TypeError):
            reasons.append('invalid managed archive marker')
    if retention.explicitly_pinned(ROOT, path):
        reasons.append('explicitly pinned')
    info = path.lstat()
    return {'path': str(path), 'device': info.st_dev, 'inode': info.st_ino,
            'marker_sha256': retention.file_digest(marker).hex() if marker.is_file() and not marker.is_symlink() else None,
            'classification': 'protected' if reasons else 'candidate for further review',
            'reasons': reasons, 'authorized_for_removal': False,
            'remaining_checks': ['ZIP matches paired signed bundle', 'verified compact source and resources',
                                 'no open or running process', 'explicit pins and newest archives']}


def generate():
    """Caller holds the project lock; includes exact paths and protections."""
    report = inventory.collect(ROOT)
    grouped = {}
    for value in report['categories']['stage']['paths']:
        path = Path(value)
        grouped.setdefault(path.parent, []).append(path)
    stages_plan = []
    for stages in grouped.values():
        stages.sort(key=lambda p: p.lstat().st_mtime_ns, reverse=True)
        keep = set(stages[:2])
        stages_plan.extend(stage_proposal(path, keep) for path in stages)
    grouped = {}
    for value in report['categories']['snapshot']['paths']:
        path = Path(value)
        grouped.setdefault(path.parent, []).append(path)
    snapshots = []
    for paths in grouped.values():
        paths.sort(key=lambda p: p.lstat().st_mtime_ns, reverse=True)
        keep = set(paths[:2])
        snapshots.extend(snapshot_proposal(path, keep) for path in paths)
    rollback_paths = [Path(path) for path in report['categories']['rollback']['paths']]
    rollback_paths.sort(key=lambda p: p.lstat().st_mtime_ns, reverse=True)
    rollbacks = [rollback_proposal(path, set(rollback_paths[:1])) for path in rollback_paths]
    archive_paths = [Path(path) for path in report['categories']['archive']['paths']]
    archive_paths.sort(key=lambda p: p.lstat().st_mtime_ns, reverse=True)
    archives = [archive_proposal(path, set(archive_paths[:2])) for path in archive_paths]
    return {'schema': 1, 'budget': report['policy'],
            'allocated_bytes': report['total_allocated_bytes'], 'exceeded': report['exceeded'],
            'status': 'proposal only; no deletion or archive operation authorized',
            'stages': stages_plan, 'snapshots': snapshots, 'rollbacks': rollbacks, 'archives': archives,
            'archives_preserved': report['categories']['archive']['paths'],
            'rollbacks_preserved': report['categories']['rollback']['paths'],
            'note': 'Legacy archives and rollback bundles are protected.'}


def main():
    if len(sys.argv) != 1:
        raise SystemExit('Usage: python3 scripts/artifact_plan.py')
    with retention.project_lock(ROOT):
        print(json.dumps(generate(), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError) as error:
        print('ERROR: ' + str(error), file=sys.stderr)
        sys.exit(1)
