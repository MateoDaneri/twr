#!/usr/bin/env python3
"""Mark an exact reviewed candidate or delivered ZIP as no longer in flight.

This never deletes artifacts. Use only after completing the candidate review or
after the requested ZIP is delivered; do not infer completion from packaging.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('rust_workspace', ROOT / 'scripts/rust_workspace.py')
workspace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workspace)
retention = workspace.retention


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--receipt', type=Path, required=True)
    parser.add_argument('--archive-delivered', action='store_true',
                        help='Also release the ZIP only after its requested delivery is complete')
    args = parser.parse_args()
    if args.receipt.is_symlink() or not args.receipt.is_file():
        raise RuntimeError('Expected an exact package receipt')
    data = json.loads(args.receipt.read_text())
    bundle = Path(data['bundle'])
    if not bundle.is_absolute() or '..' in bundle.parts or bundle.resolve(strict=True) != bundle:
        raise RuntimeError('Candidate path must be absolute and contain no symlinks')
    with retention.project_lock(ROOT):
        _, runtimes = retention.project_storage(ROOT)
        matching = [runtime for runtime in runtimes if bundle.is_relative_to(runtime)]
        if len(matching) != 1:
            raise RuntimeError('Candidate is not in a registered project runtime')
        with workspace.exclusive_build_lock(matching[0]):
            subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(bundle)],
                           check=True, timeout=30, capture_output=True)
            released = retention.finish_managed_stage(ROOT, bundle,
                        expected_source=data['source_sha256'], expected_binary=data['signed_binary_sha256'])
            if not released:
                raise RuntimeError('Candidate is legacy or has no managed marker')
            archive_released = False
            if args.archive_delivered:
                if not data.get('archive') or not data.get('archive_sha256'):
                    raise RuntimeError('Receipt does not identify an archive')
                archive_released = retention.finish_managed_archive(ROOT, Path(data['archive']),
                                   expected_source=data['source_sha256'], expected_archive=data['archive_sha256'])
                if not archive_released:
                    raise RuntimeError('Archive is legacy or has no managed marker')
    print(json.dumps({'candidate_released': released, 'archive_released': archive_released,
                      'deleted': False}, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit('ERROR: ' + str(error))
