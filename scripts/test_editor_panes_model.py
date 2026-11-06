#!/usr/bin/env python3
"""Compile and test the standalone pane model. No Cargo, GUI or user profiles."""
import argparse
import hashlib
import json
import os
import stat
import subprocess
import tempfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rustc', type=Path, required=True, help='Previously verified Rust1.99.1 compiler path')
    args = parser.parse_args()
    compiler = args.rustc
    if not compiler.is_absolute() or compiler.is_symlink():
        raise RuntimeError('Use the absolute verified compiler executable, without a symlink')
    info = compiler.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise RuntimeError('Compiler must be a regular owned executable, not writable by others')
    root = Path(__file__).resolve().parents[1]
    source = root/'crates/workspace-core/src/editor_panes.rs'
    fixture = root/'Resources/Fixtures/nvim-editor-panes.rs'
    spatial = root/'crates/workspace-core/src/editor_pane_focus.rs'
    spatial_fixture = root/'Resources/Fixtures/nvim-editor-pane-focus.rs'
    hashes = {}
    for path in (source, fixture, spatial, spatial_fixture, root/'Resources/Fixtures/nvim-editor-panes.json', root/'Resources/Fixtures/nvim-editor-pane-focus.json', root/'Resources/Fixtures/nvim-editor-panes-independent.rs', root/'Resources/Fixtures/nvim-editor-panes-independent.json'):
        # The geometry reference is 1.7 MiB; retain a bounded input check.
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 2*1024*1024:
            raise RuntimeError('Unexpected model input: '+str(path))
        hashes[str(path.relative_to(root))] = hashlib.sha256(path.read_bytes()).hexdigest()
    os.umask(0o077)
    stage = Path(tempfile.mkdtemp(prefix='tw-editor-model-tests-', dir='/private/tmp'))
    stage.chmod(0o700)
    env = {'HOME':str(stage),'TMPDIR':str(stage),'PATH':'/usr/bin:/bin','LC_ALL':'C'}
    version = subprocess.check_output([str(compiler),'--version'],env=env,cwd=stage,
                                      stderr=subprocess.STDOUT,timeout=3).decode().strip()
    if not version.startswith('rustc 1.99.1 '):
        raise RuntimeError('Expected the pinned Rust1.99.1 toolchain')
    with (stage/'output.log').open('xb') as log:
        subprocess.run([str(compiler),'--edition','2024','-D','warnings','--test',str(source),
                        '-o',str(stage/'tests')],env=env,cwd=stage,stdout=log,stderr=log,
                       check=True,timeout=30)
        subprocess.run([str(stage/'tests'),'--test-threads=1','--nocapture'],env=env,cwd=stage,
                       stdout=log,stderr=log,check=True,timeout=15)
    receipt = {'scope':'standalone model only, not GUI or release','compiler':version,
               'inputs':hashes,'result':'PASS','stage':str(stage)}
    (stage/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
    print(json.dumps(receipt),flush=True)


if __name__ == '__main__':
    main()
