#!/usr/bin/env python3
"""Install the pinned official Rust components in the private runtime cache.

Preserves the previous project-local toolchain. Never changes signatures,
extended attributes, Gatekeeper, global PATH or the user's Rust installation.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import platform
import re
import subprocess
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('driver', ROOT / 'scripts/rust_workspace.py')
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


def fetch(url, path, limit):
    if not url.startswith('https://static.rust-lang.org/dist/'):
        raise RuntimeError('Only official Rust distribution URLs are accepted')
    total = 0
    with urlopen(url, timeout=45) as response, path.open('wb') as out:
        while True:
            block = response.read(1024 * 1024)
            if not block:
                break
            total += len(block)
            if total > limit:
                raise RuntimeError('Official component exceeds download limit')
            out.write(block)


COMPONENTS = ['rustc', 'cargo', 'rust-std', 'rustfmt-preview', 'clippy-preview', 'llvm-tools-preview']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--component', action='append', choices=COMPONENTS, help='Install only a selected official component; repeat as needed')
    args = parser.parse_args()
    if (platform.system(), platform.machine()) != ('Darwin', 'arm64'):
        raise RuntimeError('This pinned toolchain requires macOS arm64')
    driver.run(['/usr/bin/true'])  # Validate/create the private build directory.
    work = driver.BUILD / 'toolchain-repair'
    work.mkdir(mode=0o700, exist_ok=True)
    if work.is_symlink() or driver.PINNED_BIN.parent.is_symlink():
        raise RuntimeError('Toolchain paths must not be symlinks')
    name = 'channel-rust-' + driver.RUST_VERSION + '.toml'
    manifest = work / name
    checksum = work / (name + '.sha256')
    fetch('https://static.rust-lang.org/dist/' + name, manifest, 4 * 1024 * 1024)
    fetch('https://static.rust-lang.org/dist/' + name + '.sha256', checksum, 1024)
    expected = checksum.read_text().split()[0]
    if not re.fullmatch('[0-9a-f]{64}', expected) or hashlib.sha256(manifest.read_bytes()).hexdigest() != expected:
        raise RuntimeError('Official manifest checksum mismatch')
    source = manifest.read_text()
    receipts = []
    for component in args.component or COMPONENTS:
        block = re.search(r'^\[pkg\.' + re.escape(component) + r'\.target\.aarch64-apple-darwin\]\n(.*?)(?=\n\[|\Z)', source, re.M | re.S)
        if not block:
            raise RuntimeError('Missing official component ' + component)
        url = re.search(r'^xz_url = "([^"]+)"', block[1], re.M)[1]
        sha = re.search(r'^xz_hash = "([0-9a-f]{64})"', block[1], re.M)[1]
        archive = work / Path(url).name
        if not archive.is_file() or hashlib.sha256(archive.read_bytes()).hexdigest() != sha:
            print('Downloading ' + component, flush=True)
            fetch(url, archive, 256 * 1024 * 1024)
        if hashlib.sha256(archive.read_bytes()).hexdigest() != sha:
            raise RuntimeError('Official component checksum mismatch: ' + component)
        extract = work / (archive.name + '.extracted')
        extract.mkdir(mode=0o700, exist_ok=True)
        subprocess.run(['/usr/bin/tar', '-xf', str(archive), '-C', str(extract)], check=True, timeout=90)
        installer = extract / archive.name.removesuffix('.tar.xz') / 'install.sh'
        subprocess.run(['/bin/sh', str(installer), '--prefix=' + str(driver.PINNED_BIN.parent), '--disable-ldconfig'], check=True, timeout=90)
        receipts.append({'component': component, 'url': url, 'sha256': sha})
    receipt_path = work / 'installed-receipt.json'
    if receipt_path.is_file() and not receipt_path.is_symlink():
        previous = json.loads(receipt_path.read_text())
        if previous.get('manifest_sha256') == expected and previous.get('prefix') == str(driver.PINNED_BIN.parent):
            selected = {r['component'] for r in receipts}
            receipts = [r for r in previous.get('components', []) if r.get('component') not in selected] + receipts
    result = {'manifest_sha256': expected, 'prefix': str(driver.PINNED_BIN.parent), 'components': receipts}
    (work / 'installed-receipt.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2), flush=True)
    for binary in ['rustc', 'cargo', 'rustfmt', 'clippy-driver']:
        subprocess.run([str(driver.PINNED_BIN / binary), '--version'], check=True, timeout=30)
    if 'llvm-tools-preview' in (args.component or COMPONENTS):
        objcopy = driver.PINNED_BIN.parent / 'lib/rustlib/aarch64-apple-darwin/bin/rust-objcopy'
        subprocess.run([str(objcopy), '--version'], check=True, timeout=30)


if __name__ == '__main__':
    main()
