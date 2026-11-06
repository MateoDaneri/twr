#!/usr/bin/env python3
"""Retire one explicitly reviewed stage from an exact inventory proposal.

This command is never called by build, package or install. Its separate use
requires authorization for the exact plan and path; a hash alone is not consent.
"""
import argparse
import hashlib
import importlib.util
import json
import os
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


def run(plan_path, plan_hash, stage_name):
    if plan_path.is_symlink() or not plan_path.is_file():
        raise RuntimeError('Expected a real reviewed plan')
    raw = plan_path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != plan_hash:
        raise RuntimeError('Plan changed since review')
    plan = json.loads(raw)
    if plan.get('schema') != 1:
        raise RuntimeError('Unrecognized plan')
    matches = [item for item in plan.get('stages', []) if item.get('path') == stage_name]
    if len(matches) != 1 or matches[0].get('classification') != 'candidate for further review':
        raise RuntimeError('Stage is not a unique candidate in this plan')
    item = matches[0]
    stage = Path(stage_name)
    with retention.project_lock(ROOT):
        _, runtimes = retention.project_storage(ROOT)
        build = stage.parent.parent
        if build not in runtimes or stage.parent != build / 'staged' or stage.is_symlink() or not stage.is_dir():
            raise RuntimeError('Stage is no longer a registered managed directory')
        with workspace.exclusive_build_lock(build):
            info = stage.lstat()
            if (info.st_dev, info.st_ino) != (item.get('device'), item.get('inode')):
                raise RuntimeError('Stage identity changed after planning')
            marker = stage / '.twr-retention.json'
            if marker.is_symlink() or not marker.is_file() or retention.file_digest(marker).hex() != item.get('marker_sha256'):
                raise RuntimeError('Stage provenance changed after planning')
            data = json.loads(marker.read_text())
            stages = sorted(plan_driver.inventory.entries(stage.parent),
                            key=lambda p: p.lstat().st_mtime_ns, reverse=True)
            if stage in stages[:2] or plan_driver.stage_proposal(stage, set(stages[:2]))['reasons']:
                raise RuntimeError('Stage is protected or provenance is incomplete')
            if retention.explicitly_pinned(ROOT, stage):
                raise RuntimeError('Stage is explicitly pinned')
            bundle_name = data.get('bundle') if data.get('schema') == 2 else 'Terminal Workspace Development.app'
            if not isinstance(bundle_name, str) or Path(bundle_name).name != bundle_name or not bundle_name.endswith('.app'):
                raise RuntimeError('Invalid managed bundle name')
            bundle = stage / bundle_name
            if bundle.is_symlink() or not bundle.is_dir() or set(p.name for p in stage.iterdir()) != {bundle_name, marker.name}:
                raise RuntimeError('Stage contains unknown or incomplete data')
            # Also refuses symlinks and special files anywhere in the stage.
            stage_contents = retention.source_files(stage)
            manifest_path = bundle / 'Contents/Resources/build-manifest.json'
            executable = bundle / 'Contents/MacOS/terminal-workspace'
            if manifest_path.is_symlink() or not manifest_path.is_file() or executable.is_symlink():
                raise RuntimeError('Unexpected bundle contents')
            manifest = json.loads(manifest_path.read_text())
            if (manifest.get('source_sha256') != data.get('source_sha256')
                    or retention.file_digest(executable).hex() != data.get('binary_sha256')):
                raise RuntimeError('Bundle no longer matches its provenance')
            subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(bundle)],
                           check=True, timeout=30, capture_output=True)
            if not retention.no_open_files(stage):
                raise RuntimeError('Stage may be active; it is preserved')
            source = Path(data['source_snapshot'])
            saved = retention.archive_source_snapshot(ROOT, build, source,
                        fingerprint=workspace.native_fingerprint, expected_sha=data['source_sha256'])
            resources = retention.archive_stage_resources(ROOT, stage, bundle,
                        expected_sha=data['source_sha256'])
            # Recheck all state after the potentially slow archive operation.
            if (stage.lstat().st_dev, stage.lstat().st_ino) != (info.st_dev, info.st_ino):
                raise RuntimeError('Stage identity changed while preserving source')
            if (retention.file_digest(marker).hex() != item['marker_sha256']
                    or retention.source_files(stage) != stage_contents or not retention.no_open_files(stage)):
                raise RuntimeError('Stage changed or became active; it is preserved')
            if retention.explicitly_pinned(ROOT, stage):
                raise RuntimeError('Stage became pinned; it is preserved')
            journal = build / 'retention-actions.jsonl'
            if journal.exists() and journal.lstat().st_size > 5 * 1024**2:
                raise RuntimeError('Retention journal needs review before another action')
            quarantine = stage.parent / ('.retiring-' + uuid.uuid4().hex)
            event = {'kind': 'stage', 'path': str(stage), 'quarantine': str(quarantine),
                     'source_history': saved, 'resource_history': resources,
                     'source_sha256': data['source_sha256'],
                     'signed_binary_sha256': data['binary_sha256'], 'build_manifest': manifest,
                     'plan_sha256': plan_hash, 'plan_entry': item}
            retention.journal_preflight(ROOT, build, [{'action': action, **event}
                                        for action in ('intent', 'removed', 'removal_incomplete')])
            retention.record(build, {'action': 'intent', **event})
            stage.rename(quarantine)
            try:
                if not retention.no_open_files(quarantine):
                    raise RuntimeError('Stage became active while retiring')
            except BaseException:
                if not stage.exists() and quarantine.exists():
                    quarantine.rename(stage)
                raise
            try:
                shutil.rmtree(quarantine)
            except OSError as error:
                retention.record(build, {'action': 'removal_incomplete', **event})
                raise RuntimeError('Partial stage retained at ' + str(quarantine) + '; inspect journal') from error
            retention.record(build, {'action': 'removed', **event})
            return event


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reviewed-plan', type=Path, required=True)
    parser.add_argument('--plan-sha256', required=True)
    parser.add_argument('--stage', required=True, help='exact single stage path from the reviewed plan')
    args = parser.parse_args()
    print(json.dumps(run(args.reviewed_plan, args.plan_sha256, args.stage), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit('ERROR: ' + str(error))
