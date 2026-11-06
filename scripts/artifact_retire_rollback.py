#!/usr/bin/env python3
"""Retire one managed rollback only after preserving its source/resources."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import uuid

ROOT = Path(__file__).resolve().parents[1]

def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

plan_driver = load('artifact_plan')
retention = plan_driver.retention
workspace = load('rust_workspace')


def preserved_source(source_sha, runtimes):
    """Prefer live managed snapshots; fall back to verified source history."""
    for runtime in sorted(runtimes, key=str):
        receipts = list(runtime.glob('build-receipt*.json'))
        markers = list((runtime / 'staged').glob('*/.twr-retention.json'))
        for path in receipts + markers:
            if path.is_symlink():
                continue
            try:
                data = json.loads(path.read_text())
            except (OSError, ValueError):
                continue
            source = Path(data.get('source_snapshot', ''))
            if (data.get('source_sha256') != source_sha or source.parent != runtime / 'source-snapshots'
                    or source.is_symlink() or not source.is_dir()):
                continue
            with workspace.exclusive_build_lock(runtime):
                return retention.archive_source_snapshot(ROOT, runtime, source,
                            fingerprint=workspace.native_fingerprint, expected_sha=source_sha)
    history = retention.common_git_dir(ROOT) / 'twr-source-history'
    manifests = history / 'manifests'
    if history.is_symlink() or manifests.is_symlink() or not manifests.is_dir():
        raise RuntimeError('No verified source snapshot or source history for rollback')
    for path in sorted(manifests.glob('*.json')):
        if path.is_symlink() or not re.fullmatch(r'[0-9a-f]{64}\.json', path.name) or path.stat().st_size > 16 * 1024**2:
            continue
        try:
            raw = path.read_bytes()
            data = json.loads(raw)
        except (OSError, ValueError):
            continue
        if (hashlib.sha256(raw).hexdigest() != path.stem or data.get('schema') != 1
                or data.get('tree_kind') != 'source' or data.get('native_source_sha256') != source_sha):
            continue
        entries = data.get('entries')
        if not isinstance(entries, dict):
            continue
        sizes = [item.get('size') for item in entries.values()
                 if isinstance(item, dict) and item.get('kind') == 'file']
        if any(not isinstance(size, int) or not 0 <= size <= 32 * 1024**2 for size in sizes):
            continue
        reserve = sum(sizes) + len(entries) * 8192 + 16 * 1024**2
        usage = plan_driver.inventory.managed_usage(ROOT, plan_driver.inventory.collect(ROOT), initialize=False)
        if (usage['allocated_bytes'] + reserve > usage['budget_bytes']
                or retention.allocated_bytes([history]) + reserve > 512 * 1024**2):
            raise RuntimeError('Insufficient history budget for rollback source verification')
        recoveries = history / 'recoveries'
        if recoveries.is_symlink() or not recoveries.is_dir():
            raise RuntimeError('Private recovery directory is unavailable')
        destination, recovery_marker = retention.register_recovery(history, 'twr-rollback-source-')
        load('artifact_restore_source').restore(path, destination)
        if workspace.native_fingerprint(destination)[0] != source_sha:
            raise RuntimeError('Restored rollback source differs; retained at ' + str(destination))
        retention.finish_recovery(destination, recovery_marker)
        return {'path': str(path), 'sha256': path.stem,
                'native_source_sha256': source_sha, 'store': str(history)}
    raise RuntimeError('No verified source history for rollback')


def run(plan_path, plan_hash, rollback_name):
    if plan_path.is_symlink() or not plan_path.is_file():
        raise RuntimeError('Expected a real reviewed plan')
    raw = plan_path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != plan_hash:
        raise RuntimeError('Plan changed since review')
    plan = json.loads(raw)
    if plan.get('schema') != 1:
        raise RuntimeError('Unrecognized plan')
    matches = [item for item in plan.get('rollbacks', []) if item.get('path') == rollback_name]
    if len(matches) != 1 or matches[0].get('classification') != 'candidate for further review':
        raise RuntimeError('Rollback is not a unique candidate in this plan')
    item = matches[0]
    rollback = Path(rollback_name)
    with retention.project_lock(ROOT):
        worktrees, runtimes = retention.project_storage(ROOT)
        parents = [worktree / 'dist/installed-backups' for worktree in worktrees]
        if rollback.parent not in parents or rollback.is_symlink() or not rollback.is_dir():
            raise RuntimeError('Rollback is no longer in a registered worktree')
        info = rollback.lstat()
        if (info.st_dev, info.st_ino) != (item.get('device'), item.get('inode')):
            raise RuntimeError('Rollback identity changed after planning')
        marker = rollback / '.twr-rollback.json'
        if marker.is_symlink() or not marker.is_file() or retention.file_digest(marker).hex() != item.get('marker_sha256'):
            raise RuntimeError('Rollback marker changed after planning')
        report = plan_driver.inventory.collect(ROOT)
        paths = [Path(path) for path in report['categories']['rollback']['paths']]
        paths.sort(key=lambda p: p.lstat().st_mtime_ns, reverse=True)
        if plan_driver.rollback_proposal(rollback, set(paths[:1]))['reasons']:
            raise RuntimeError('Rollback is protected or provenance is incomplete')
        data = json.loads(marker.read_text())
        bundle = rollback / data['bundle']
        if bundle.is_symlink() or not bundle.is_dir() or set(p.name for p in rollback.iterdir()) != {bundle.name, marker.name}:
            raise RuntimeError('Rollback contains unknown or incomplete data')
        original = retention.source_files(rollback)
        manifest = bundle / 'Contents/Resources/build-manifest.json'
        executable = bundle / 'Contents/MacOS/terminal-workspace'
        if manifest.is_symlink() or not manifest.is_file() or executable.is_symlink():
            raise RuntimeError('Rollback manifest or executable unavailable')
        provenance = json.loads(manifest.read_text())
        source_sha = provenance.get('source_sha256')
        if (not isinstance(source_sha, str) or not re.fullmatch(r'[0-9a-f]{64}', source_sha)
                or retention.file_digest(executable).hex() != data['signed_binary_sha256']):
            raise RuntimeError('Rollback provenance changed')
        subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(bundle)],
                       check=True, timeout=30, capture_output=True)
        if not retention.no_open_files(rollback):
            raise RuntimeError('Rollback may be active; it is preserved')
        saved = preserved_source(source_sha, runtimes)
        resources = retention.archive_stage_resources(ROOT, rollback, bundle, expected_sha=source_sha)
        if ((rollback.lstat().st_dev, rollback.lstat().st_ino) != (info.st_dev, info.st_ino)
                or retention.file_digest(marker).hex() != item['marker_sha256']
                or retention.source_files(rollback) != original
                or retention.explicitly_pinned(ROOT, rollback) or not retention.no_open_files(rollback)):
            raise RuntimeError('Rollback changed or became active; it is preserved')
        report = plan_driver.inventory.collect(ROOT)
        current = [Path(path) for path in report['categories']['rollback']['paths']]
        current.sort(key=lambda p: p.lstat().st_mtime_ns, reverse=True)
        if rollback in current[:1]:
            raise RuntimeError('Rollback became the newest; it is preserved')
        common = retention.common_git_dir(ROOT)
        journal = common / 'retention-actions.jsonl'
        if journal.exists() and journal.lstat().st_size > 5 * 1024**2:
            raise RuntimeError('Retention journal needs review before another action')
        quarantine = rollback.parent / ('.retiring-' + uuid.uuid4().hex)
        event = {'kind': 'rollback', 'path': str(rollback), 'quarantine': str(quarantine),
                 'source_history': saved, 'resource_history': resources, 'source_sha256': source_sha,
                 'signed_binary_sha256': data['signed_binary_sha256'], 'build_manifest': provenance,
                 'plan_sha256': plan_hash, 'plan_entry': item}
        retention.journal_preflight(ROOT, common, [{'action': action, **event}
                                    for action in ('intent', 'removed', 'removal_incomplete')])
        retention.record(common, {'action': 'intent', **event})
        rollback.rename(quarantine)
        try:
            if not retention.no_open_files(quarantine):
                raise RuntimeError('Rollback became active while retiring')
        except BaseException:
            if not rollback.exists() and quarantine.exists():
                quarantine.rename(rollback)
            raise
        try:
            shutil.rmtree(quarantine)
        except OSError as error:
            retention.record(common, {'action': 'removal_incomplete', **event})
            raise RuntimeError('Partial rollback retained at ' + str(quarantine) + '; inspect journal') from error
        retention.record(common, {'action': 'removed', **event})
        return event


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reviewed-plan', type=Path, required=True)
    parser.add_argument('--plan-sha256', required=True)
    parser.add_argument('--rollback', required=True)
    args = parser.parse_args()
    print(json.dumps(run(args.reviewed_plan, args.plan_sha256, args.rollback), indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit('ERROR: ' + str(error))
