#!/usr/bin/env python3
"""Retire one reviewed source snapshot only after verified compact archival.

Never invoked automatically. A hash is an integrity check, not user consent.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import uuid

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('artifact_plan', ROOT / 'scripts/artifact_plan.py')
plan_driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plan_driver)
retention = plan_driver.retention
spec = importlib.util.spec_from_file_location('rust_workspace', ROOT / 'scripts/rust_workspace.py')
workspace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workspace)


def run(plan_path, plan_hash, source_name):
    if plan_path.is_symlink() or not plan_path.is_file():
        raise RuntimeError('Expected a real reviewed plan')
    raw = plan_path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != plan_hash:
        raise RuntimeError('Plan changed since review')
    plan = json.loads(raw)
    if plan.get('schema') != 1:
        raise RuntimeError('Unrecognized plan')
    matches = [item for item in plan.get('snapshots', []) if item.get('path') == source_name]
    if len(matches) != 1 or matches[0].get('classification') != 'candidate for further review':
        raise RuntimeError('Snapshot is not a unique candidate in this plan')
    item = matches[0]
    source = Path(source_name)
    with retention.project_lock(ROOT):
        _, runtimes = retention.project_storage(ROOT)
        build = source.parent.parent
        if build not in runtimes or source.parent != build / 'source-snapshots' or source.is_symlink() or not source.is_dir():
            raise RuntimeError('Snapshot is no longer a registered managed directory')
        with workspace.exclusive_build_lock(build):
            info = source.lstat()
            if (info.st_dev, info.st_ino) != (item.get('device'), item.get('inode')):
                raise RuntimeError('Snapshot identity changed after planning')
            marker = build / 'managed-snapshots' / (source.name + '.json')
            manifest = source / 'source-manifest.json'
            if (marker.parent.is_symlink() or marker.is_symlink() or not marker.is_file()
                    or retention.file_digest(marker).hex() != item.get('marker_sha256')
                    or manifest.is_symlink() or not manifest.is_file()
                    or retention.file_digest(manifest).hex() != item.get('manifest_sha256')):
                raise RuntimeError('Snapshot provenance changed after planning')
            marker_info = marker.lstat()
            paths = plan_driver.inventory.entries(source.parent, skip=('latest.json',))
            paths.sort(key=lambda p: p.lstat().st_mtime_ns, reverse=True)
            if plan_driver.snapshot_proposal(source, set(paths[:2]))['reasons']:
                raise RuntimeError('Snapshot is protected or provenance is incomplete')
            if retention.active_source(build, source) or retention.explicitly_pinned(ROOT, source):
                raise RuntimeError('Snapshot is current or explicitly pinned')
            original = retention.source_files(source)
            if not retention.no_open_files(source):
                raise RuntimeError('Snapshot may be active; it is preserved')
            source_sha = workspace.native_fingerprint(source)[0]
            saved = retention.archive_source_snapshot(ROOT, build, source,
                        fingerprint=workspace.native_fingerprint, expected_sha=source_sha)
            if ((source.lstat().st_dev, source.lstat().st_ino) != (info.st_dev, info.st_ino)
                    or retention.file_digest(marker).hex() != item['marker_sha256']
                    or retention.source_files(source) != original
                    or retention.active_source(build, source)
                    or retention.explicitly_pinned(ROOT, source) or not retention.no_open_files(source)):
                raise RuntimeError('Snapshot became active or changed; it is preserved')
            journal = build / 'retention-actions.jsonl'
            if journal.exists() and journal.lstat().st_size > 5 * 1024**2:
                raise RuntimeError('Retention journal needs review before another action')
            quarantine = source.parent / ('.retiring-' + uuid.uuid4().hex)
            event = {'kind': 'source_snapshot', 'path': str(source), 'quarantine': str(quarantine),
                     'source_history': saved, 'manifest_sha256': item['manifest_sha256'],
                     'plan_sha256': plan_hash, 'plan_entry': item}
            retention.journal_preflight(ROOT, build, [{'action': action, **event}
                                        for action in ('intent', 'removed', 'removal_incomplete')])
            retention.record(build, {'action': 'intent', **event})
            source.rename(quarantine)
            try:
                if (not retention.no_open_files(quarantine) or marker.is_symlink()
                        or (marker.lstat().st_dev, marker.lstat().st_ino) != (marker_info.st_dev, marker_info.st_ino)
                        or retention.file_digest(marker).hex() != item['marker_sha256']):
                    raise RuntimeError('Snapshot became active while retiring')
            except BaseException:
                if not source.exists() and quarantine.exists():
                    quarantine.rename(source)
                raise
            try:
                shutil.rmtree(quarantine)
            except OSError as error:
                retention.record(build, {'action': 'removal_incomplete', **event})
                raise RuntimeError('Partial snapshot retained at ' + str(quarantine) + '; inspect journal') from error
            # Only the exact lifecycle marker matched above may be removed.
            marker_retained = True
            try:
                if (not marker.is_symlink()
                        and (marker.lstat().st_dev, marker.lstat().st_ino) == (marker_info.st_dev, marker_info.st_ino)
                        and retention.file_digest(marker).hex() == item['marker_sha256']):
                    marker.unlink()
                    marker_retained = False
            except OSError:
                pass
            retention.record(build, {'action': 'removed', 'marker_retained': marker_retained, **event})
            return event


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reviewed-plan', type=Path, required=True)
    parser.add_argument('--plan-sha256', required=True)
    parser.add_argument('--snapshot', required=True, help='exact single snapshot path from the reviewed plan')
    args = parser.parse_args()
    print(json.dumps(run(args.reviewed_plan, args.plan_sha256, args.snapshot), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit('ERROR: ' + str(error))
