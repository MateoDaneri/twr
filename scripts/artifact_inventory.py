#!/usr/bin/env python3
"""Read-only, shared inventory of TWR's known build and package artifacts.

No artifact is removed, compressed, moved, or modified. This deliberately counts
legacy and unclassified artifacts as well as new markers; a marker never grants
permission to delete. It must be run under the shared project lock.
"""
import importlib.util
import json
import os
import re
from pathlib import Path
import stat
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('artifact_retention', ROOT / 'scripts/artifact_retention.py')
retention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(retention)

# Target limits, not a promise that protected or unclassified data can be deleted.
POLICY = {
    'total_bytes': 3 * 1024**3,
    'stage_count': 12,
    'snapshot_count': 16,
    'archive_count': 4,
    'rollback_count': 1,
    'source_history_bytes': 512 * 1024**2,
}


def entries(directory, *, skip=()):
    if directory.is_symlink():
        raise RuntimeError('Refusing symlinked artifact directory: ' + str(directory))
    if not directory.exists():
        return []
    if not directory.is_dir():
        raise RuntimeError('Artifact directory is not a directory: ' + str(directory))
    return sorted((p for p in directory.iterdir() if p.name not in skip), key=str)


def zip_files(directory):
    # Refuse symlinked directories rather than follow paths outside the project.
    if directory.is_symlink():
        raise RuntimeError('Refusing symlinked dist: ' + str(directory))
    if not directory.exists():
        return [], []
    result = []
    markers = []
    stack = [directory]
    while stack:
        current = stack.pop()
        for path in entries(current):
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode):
                continue
            if stat.S_ISDIR(mode):
                # This tree is counted separately as complete rollback bundles.
                if current == directory and path.name == 'installed-backups':
                    continue
                stack.append(path)
            elif stat.S_ISREG(mode) and path.suffix.lower() == '.zip':
                result.append(path)
            elif stat.S_ISREG(mode) and path.name.endswith('.twr-archive.json'):
                markers.append(path)
    return sorted(result, key=str), sorted(markers, key=str)


def collect(root):
    worktrees, runtimes = retention.project_storage(root)
    categories = {name: [] for name in ('stage', 'snapshot', 'archive', 'rollback',
                                         'source_history', 'fixed_bundle', 'journal', 'snapshot_markers', 'archive_markers', 'policy_metadata', 'recovery_scratch')}
    for runtime in sorted(runtimes, key=str):
        categories['stage'].extend(entries(runtime / 'staged'))
        categories['snapshot'].extend(entries(runtime / 'source-snapshots', skip=('latest.json',)))
        categories['snapshot_markers'].extend(entries(runtime / 'managed-snapshots'))
        for name in ('bundle', 'validation'):
            path = runtime / name
            if path.exists() or path.is_symlink():
                categories['fixed_bundle'].append(path)
        journal = runtime / 'retention-actions.jsonl'
        if journal.exists() or journal.is_symlink():
            categories['journal'].append(journal)
    for worktree in worktrees:
        dist = worktree / 'dist'
        archives, markers = zip_files(dist)
        categories['archive'].extend(archives)
        categories['archive_markers'].extend(markers)
        categories['rollback'].extend(entries(dist / 'installed-backups'))
        if dist.is_dir() and not dist.is_symlink():
            categories['policy_metadata'].extend(sorted(dist.glob('installed-*.json'), key=str))
    history = retention.common_git_dir(root) / 'twr-source-history'
    categories['source_history'].extend(entries(history))
    for marker in entries(history / 'recoveries'):
        if (marker.is_symlink() or not marker.is_file()
                or not re.fullmatch(r'[0-9a-f]{32}\.json', marker.name)):
            raise RuntimeError('Unclassified recovery marker; preserve and review')
        info = marker.lstat()
        if info.st_uid != os.getuid() or info.st_mode & 0o077 or info.st_size > 4096:
            raise RuntimeError('Recovery marker must be private and owned')
        data = json.loads(marker.read_text())
        path = Path(data.get('path', ''))
        if (data.get('schema') != 1 or path.parent != Path('/private/tmp')
                or not re.fullmatch(r'twr-(?:source-recovery|rollback-source)-' + marker.stem, path.name)
                or path.is_symlink()):
            raise RuntimeError('Invalid registered recovery path')
        if path.exists():
            info = path.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
                raise RuntimeError('Recovery directory must be private and owned')
            categories['recovery_scratch'].append(path)
    for name in ('twr-retention-pins.json', 'twr-retention-plan.json',
                 'twr-retention-baseline.json', 'retention-actions.jsonl'):
        path = retention.common_git_dir(root) / name
        if path.exists() or path.is_symlink():
            categories['policy_metadata'].append(path)
    # Count an inode once over the entire policy, including hardlinks. An APFS
    # clone is conservatively counted at its reported allocation per inode.
    all_paths = [path for paths in categories.values() for path in paths]
    totals = {name: {'count': len(paths), 'allocated_bytes': retention.allocated_bytes(paths),
                     'paths': [str(path) for path in paths]}
              for name, paths in categories.items()}
    total = retention.allocated_bytes(all_paths)
    exceeded = []
    for name, key in (('stage', 'stage_count'), ('snapshot', 'snapshot_count'),
                      ('archive', 'archive_count'), ('rollback', 'rollback_count')):
        if totals[name]['count'] > POLICY[key]:
            exceeded.append(key)
    if totals['source_history']['allocated_bytes'] > POLICY['source_history_bytes']:
        exceeded.append('source_history_bytes')
    if total > POLICY['total_bytes']:
        exceeded.append('total_bytes')
    return {'schema': 1, 'policy': POLICY, 'total_allocated_bytes': total,
            'exceeded': exceeded, 'categories': totals,
            'action': 'inventory only; no artifacts selected or removed',
            'limitations': ['Target limits are not enforced by this report.',
                            'Unknown non-ZIP dist files, historical exports outside registered dist, build targets, external toolchains and unregistered helper/probe scratch are not included.',
                            'Protected and unclassified entries remain counted.']}


def main():
    if len(sys.argv) != 1:
        raise SystemExit('Usage: python3 scripts/artifact_inventory.py')
    with retention.project_lock(ROOT):
        report = collect(ROOT)
        baseline = retention.common_git_dir(ROOT) / 'twr-retention-baseline.json'
        report['managed_pool'] = managed_usage(ROOT, report) if baseline.exists() else None
        print(json.dumps(report, ensure_ascii=False, indent=2))

# Newly generated artifacts have a separate hard reservation pool while the
# already-existing, protected legacy population is migrated under exact plans.
MANAGED_BUDGET = 1536 * 1024**2
METADATA_RESERVE = 8 * 1024**2
MANAGED_COUNTS = {'stage': 12, 'snapshot': 16, 'archive': 4, 'rollback': 2}


def managed_usage(root, report, *, initialize=False):
    """Count new artifacts and growth above an immutable exact legacy baseline.

    Caller holds the shared project lock. Source history, journals and policy
    metadata always count toward the new pool; they are never grandfathered.
    """
    common = retention.common_git_dir(root)
    baseline_path = common / 'twr-retention-baseline.json'
    if baseline_path.is_symlink():
        raise RuntimeError('Legacy baseline is a symlink')
    artifact_categories = ('stage', 'snapshot', 'archive', 'rollback', 'fixed_bundle')
    if not baseline_path.exists():
        if not initialize:
            raise RuntimeError('Legacy baseline has not been initialized')
        baseline = {}
        for category in artifact_categories:
            for name in report['categories'][category]['paths']:
                path = Path(name)
                info = path.lstat()
                baseline[name] = {'device': info.st_dev, 'inode': info.st_ino,
                                  'allocated_bytes': retention.allocated_bytes([path])}
        # Publish a complete file once; a crash cannot leave a partial baseline.
        data = json.dumps({'schema': 1, 'entries': baseline}, sort_keys=True, indent=2) + '\n'
        fd, name = tempfile.mkstemp(prefix='.twr-baseline-', dir=common)
        temporary = Path(name)
        try:
            with os.fdopen(fd, 'w') as output:
                output.write(data)
                output.flush()
                os.fsync(output.fileno())
            os.link(temporary, baseline_path, follow_symlinks=False)
        finally:
            if temporary.is_file() and not temporary.is_symlink():
                temporary.unlink()
    fd = os.open(baseline_path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd) as source:
        info = os.fstat(source.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_mode & 0o077 or info.st_size > 16 * 1024**2):
            raise RuntimeError('Legacy baseline is not a private owned file')
        data = json.load(source)
    if data.get('schema') != 1 or not isinstance(data.get('entries'), dict):
        raise RuntimeError('Invalid legacy baseline')
    baseline = data['entries']
    total = 0
    counts = {name: 0 for name in MANAGED_COUNTS}
    legacy = 0
    for category, entry in report['categories'].items():
        for name in entry['paths']:
            path = Path(name)
            info = path.lstat()
            size = retention.allocated_bytes([path])
            old = baseline.get(name) if category in artifact_categories else None
            if (isinstance(old, dict) and old.get('device') == info.st_dev
                    and old.get('inode') == info.st_ino
                    and isinstance(old.get('allocated_bytes'), int) and old['allocated_bytes'] >= 0):
                excluded = min(size, old['allocated_bytes'])
                legacy += excluded
                total += size - excluded
                if size > excluded and category in counts:
                    counts[category] += 1
            else:
                total += size
                if category in counts:
                    counts[category] += 1
    # The initial report predates creation of the baseline file.
    if str(baseline_path) not in report['categories']['policy_metadata']['paths']:
        total += retention.allocated_bytes([baseline_path])
    return {'budget_bytes': MANAGED_BUDGET, 'allocated_bytes': total,
            'frozen_legacy_bytes': legacy, 'counts': counts, 'limits': MANAGED_COUNTS,
            'baseline': str(baseline_path)}


def preflight(root, *, kind, reserve_bytes, archive=False):
    """Fail closed before new allocations can grow beyond the managed pool."""
    report = collect(root)
    usage = managed_usage(root, report, initialize=True)
    required = {kind: 1}
    if archive:
        required['archive'] = required.get('archive', 0) + 1
    for category, extra in required.items():
        if category in MANAGED_COUNTS and usage['counts'][category] + extra > MANAGED_COUNTS[category]:
            raise RuntimeError('Managed ' + category + ' count is full; reconcile or review the retained items')
    if usage['allocated_bytes'] + reserve_bytes + METADATA_RESERVE > MANAGED_BUDGET:
        raise RuntimeError('Managed artifact budget is full; preserve unique sources and reconcile before allocating more')
    return usage


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError) as error:
        print('ERROR: ' + str(error), file=sys.stderr)
        sys.exit(1)
