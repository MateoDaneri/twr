#!/usr/bin/env python3
"""Retire one marked ZIP after verifying its paired bundle and source history."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import uuid
import zipfile

ROOT = Path(__file__).resolve().parents[1]

def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

plan_driver = load('artifact_plan')
retention = plan_driver.retention
workspace = load('rust_workspace')


def verify_zip(archive, bundle, contents):
    expected = {bundle.name + '/' + name: value[-1].hex()
                for name, value in contents.items() if name and value[0] == 'file'}
    with zipfile.ZipFile(archive) as source:
        seen = set()
        total = 0
        for item in source.infolist():
            name = item.filename
            if item.is_dir() or name in seen or name not in expected or item.flag_bits & 1:
                raise RuntimeError('Unexpected ZIP entry')
            seen.add(name)
            total += item.file_size
            if item.file_size > 64 * 1024**2 or total > 256 * 1024**2:
                raise RuntimeError('ZIP exceeds managed bounds')
            digest = hashlib.sha256()
            with source.open(item) as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                    digest.update(chunk)
            if digest.hexdigest() != expected[name]:
                raise RuntimeError('ZIP differs from paired bundle')
        if seen != set(expected):
            raise RuntimeError('ZIP is missing a bundle file')


def run(plan_path, plan_hash, archive_name):
    if plan_path.is_symlink() or not plan_path.is_file():
        raise RuntimeError('Expected a real reviewed plan')
    raw = plan_path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != plan_hash:
        raise RuntimeError('Plan changed since review')
    plan = json.loads(raw)
    if plan.get('schema') != 1:
        raise RuntimeError('Unrecognized plan')
    matches = [item for item in plan.get('archives', []) if item.get('path') == archive_name]
    if len(matches) != 1 or matches[0].get('classification') != 'candidate for further review':
        raise RuntimeError('Archive is not a unique candidate in this plan')
    item = matches[0]
    archive = Path(archive_name)
    with retention.project_lock(ROOT):
        worktrees, runtimes = retention.project_storage(ROOT)
        if (archive.is_symlink() or not archive.is_file() or archive.resolve(strict=True) != archive or not any(
                archive.is_relative_to(worktree / 'dist') for worktree in worktrees)):
            raise RuntimeError('Archive is outside registered worktrees')
        info = archive.lstat()
        if (info.st_dev, info.st_ino) != (item.get('device'), item.get('inode')):
            raise RuntimeError('Archive identity changed after planning')
        marker = archive.with_name(archive.name + '.twr-archive.json')
        if marker.is_symlink() or not marker.is_file() or retention.file_digest(marker).hex() != item.get('marker_sha256'):
            raise RuntimeError('Archive marker changed after planning')
        marker_info = marker.lstat()
        data = json.loads(marker.read_text())
        bundle = Path(data.get('bundle', ''))
        matches = [runtime for runtime in runtimes if bundle.is_relative_to(runtime)]
        if (len(matches) != 1 or bundle.is_symlink() or not bundle.is_dir()
                or bundle.resolve(strict=True) != bundle):
            raise RuntimeError('Paired bundle is unavailable')
        runtime = matches[0]
        with workspace.exclusive_build_lock(runtime):
            report = plan_driver.inventory.collect(ROOT)
            paths = [Path(path) for path in report['categories']['archive']['paths']]
            paths.sort(key=lambda p: p.lstat().st_mtime_ns, reverse=True)
            if plan_driver.archive_proposal(archive, set(paths[:2]))['reasons']:
                raise RuntimeError('Archive is protected or provenance incomplete')
            if retention.file_digest(archive, 256 * 1024**2).hex() != data['archive_sha256']:
                raise RuntimeError('Archive digest changed')
            contents = retention.source_files(bundle)
            manifest_file = bundle / 'Contents/Resources/build-manifest.json'
            executable = bundle / 'Contents/MacOS/terminal-workspace'
            manifest = json.loads(manifest_file.read_text())
            if (manifest.get('source_sha256') != data.get('source_sha256')
                    or retention.file_digest(executable).hex() != data.get('signed_binary_sha256')):
                raise RuntimeError('Paired bundle differs from archive provenance')
            subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(bundle)],
                           check=True, timeout=30, capture_output=True)
            if not retention.no_open_files(bundle) or not retention.no_open_file(archive):
                raise RuntimeError('Archive or paired bundle may be active')
            verify_zip(archive, bundle, contents)
            source = Path(data.get('source_snapshot', ''))
            saved = retention.archive_source_snapshot(ROOT, runtime, source,
                        fingerprint=workspace.native_fingerprint, expected_sha=data['source_sha256'])
            resources = retention.archive_stage_resources(ROOT, bundle.parent, bundle,
                        expected_sha=data['source_sha256'])
            if ((archive.lstat().st_dev, archive.lstat().st_ino) != (info.st_dev, info.st_ino)
                    or retention.file_digest(marker).hex() != item['marker_sha256']
                    or retention.file_digest(archive, 256 * 1024**2).hex() != data['archive_sha256']
                    or retention.source_files(bundle) != contents or retention.explicitly_pinned(ROOT, archive)
                    or not retention.no_open_files(bundle) or not retention.no_open_file(archive)):
                raise RuntimeError('Archive or paired bundle changed; preserved')
            common = retention.common_git_dir(ROOT)
            event = {'kind': 'archive', 'path': str(archive), 'archive_sha256': data['archive_sha256'],
                     'source_history': saved, 'resource_history': resources,
                     'source_sha256': data['source_sha256'], 'signed_binary_sha256': data['signed_binary_sha256'],
                     'build_manifest': manifest, 'plan_sha256': plan_hash, 'plan_entry': item}
            retention.journal_preflight(ROOT, common, [{'action': action, **event}
                                        for action in ('intent', 'removed', 'removal_incomplete')])
            retention.record(common, {'action': 'intent', **event})
            quarantine = archive.with_name('.retiring-' + uuid.uuid4().hex + '.zip')
            archive.rename(quarantine)
            try:
                if (not retention.no_open_file(quarantine) or marker.is_symlink()
                        or (marker.lstat().st_dev, marker.lstat().st_ino) != (marker_info.st_dev, marker_info.st_ino)
                        or retention.file_digest(marker).hex() != item['marker_sha256']):
                    raise RuntimeError('Archive became active while retiring')
            except BaseException:
                if not archive.exists() and quarantine.exists():
                    quarantine.rename(archive)
                raise
            try:
                quarantine.unlink()
            except OSError as error:
                retention.record(common, {'action': 'removal_incomplete', **event})
                raise RuntimeError('Partial archive retained at ' + str(quarantine)) from error
            marker_retained = True
            try:
                if (not marker.is_symlink()
                        and (marker.lstat().st_dev, marker.lstat().st_ino) == (marker_info.st_dev, marker_info.st_ino)
                        and retention.file_digest(marker).hex() == item['marker_sha256']):
                    marker.unlink()
                    marker_retained = False
            except OSError:
                pass
            retention.record(common, {'action': 'removed', 'marker_retained': marker_retained, **event})
            return event


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reviewed-plan', type=Path, required=True)
    parser.add_argument('--plan-sha256', required=True)
    parser.add_argument('--archive', required=True)
    args = parser.parse_args()
    print(json.dumps(run(args.reviewed_plan, args.plan_sha256, args.archive), indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        raise SystemExit('ERROR: ' + str(error))
