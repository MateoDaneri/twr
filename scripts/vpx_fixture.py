#!/usr/bin/env python3
"""Pinned temporary VP8/VP9 encoder and synthetic two-second WebM fixtures.

No installation, user media, desktop capture or application runtime dependency.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import stat
import subprocess
import tempfile

spec = importlib.util.spec_from_file_location('bounded', Path(__file__).with_name('ghostty_probe.py'))
bounded = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bounded)
VERSION = '1.17.0'
URL = 'https://codeload.github.com/webmproject/libvpx/tar.gz/refs/tags/v' + VERSION
SHA = '1020f184046187baa2985dbde38e0691f49c44088bca7a1842b0236c6081dc0a'
FLAGS = ['--target=generic-gnu', '--disable-docs', '--disable-unit-tests',
         '--disable-libyuv', '--enable-webm-io', '--disable-vp8-decoder', '--disable-vp9-decoder']


def prepare():
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise RuntimeError('This fixture builder targets macOS arm64')
    if shutil.disk_usage('/private/tmp').free < 5 * 1024**3 // 2:
        raise RuntimeError('Prepare requires2.5GiB free; nothing deleted')
    for name in ['/usr/bin/clang', '/usr/bin/make']:
        if not os.access(name, os.X_OK):
            raise RuntimeError('Missing required executable: ' + name)
    stage = Path(tempfile.mkdtemp(prefix='tw-vpx-fixture-', dir='/private/tmp'))
    print(stage, flush=True)
    receipt = {'version': VERSION, 'flags': FLAGS, 'source': bounded.get(URL, stage / 'source.tar.gz', 16 * 1024**2, SHA)}
    receipt['extracted'] = bounded.extract(stage / 'source.tar.gz', stage / 'source', 'libvpx-' + VERSION, 128 * 1024**2)
    receipt['extracted'].pop('blobs', None)
    for name, command, seconds in [('configure', ['/bin/sh', './configure', *FLAGS], 40),
                                    ('build', ['/usr/bin/make', '-j2'], 480)]:
        receipt[name] = bounded.run_owned(stage, command, stage / (name + '.log'), timeout=seconds)
        (stage / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
        result = receipt[name]
        if result['returncode'] or result['stopped'] or result['cleanup_errors']:
            raise RuntimeError('Encoder build failed; stage preserved')
    receipt['encoder_sha256'] = bounded.sha256(stage / 'source/vpxenc')
    (stage / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    return stage


def checked(stage):
    stage = Path(stage)
    info = stage.lstat()
    if stage.resolve(strict=True) != stage or stage.parent != Path('/private/tmp') or not stage.name.startswith('tw-vpx-fixture-') or not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('Expected an owned private tw-vpx-fixture-* stage')
    for relative, limit in [('receipt.json', 1024**2), ('source.tar.gz', 16 * 1024**2), ('source/vpxenc', 64 * 1024**2)]:
        info = bounded.child(stage, stage / relative).lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > limit:
            raise RuntimeError('Invalid prepared encoder input: ' + relative)
    receipt = json.loads((stage / 'receipt.json').read_text())
    if receipt['version'] != VERSION or receipt['flags'] != FLAGS or receipt['source']['sha256'] != SHA or bounded.sha256(bounded.child(stage, stage / 'source.tar.gz')) != SHA:
        raise RuntimeError('Encoder provenance changed')
    for name in ['configure', 'build']:
        result = receipt[name]
        if result['returncode'] or result['stopped'] or result['cleanup_errors']:
            raise RuntimeError('Encoder supervision did not pass')
    encoder = bounded.child(stage, stage / 'source/vpxenc')
    if bounded.sha256(encoder) != receipt['encoder_sha256']:
        raise RuntimeError('Encoder binary changed')
    return stage, encoder


def fixture(stage):
    stage, encoder = checked(stage)
    output = Path(tempfile.mkdtemp(prefix='samples-', dir=stage))
    source = output / 'synthetic.y4m'
    # Direct planar YUV420 pixels: moving red box, yellow progress, blue field.
    with source.open('xb') as stream:
        stream.write(b'YUV4MPEG2 W320 H180 F12:1 Ip A1:1 C420jpeg\n')
        for frame in range(24):
            planes = [bytearray() for _ in range(3)]
            for plane, scale in [(0, 1), (1, 2), (2, 2)]:
                for y in range(0, 180, scale):
                    for x in range(0, 320, scale):
                        color = (81, 90, 240) if frame * 11 <= x < frame * 11 + 48 and 45 <= y < 95 else (210, 16, 146) if y >= 150 and x < (frame + 1) * 320 // 24 else (50, 160, 110)
                        planes[plane].append(color[plane])
            stream.write(b'FRAME\n' + b''.join(planes))
    receipt = {'scope': '24 synthetic frames,320x180,12fps,2s; no user data/network', 'encoder_stage': str(stage), 'input_sha256': bounded.sha256(source), 'samples': []}
    for codec in ['vp8', 'vp9']:
        target = output / (codec + '.webm')
        command = [str(encoder), '--codec=' + codec, '--webm', '--good', '--cpu-used=8', '--threads=2', '--passes=1', '--limit=24', '--debug', '-o', str(target), str(source)]
        with (output / (codec + '.log')).open('xb') as log:
            subprocess.run(command, cwd=output, env=bounded.environment(stage), stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT, timeout=30, check=True)
        if not target.is_file() or target.stat().st_size > 1024**2:
            raise RuntimeError('Fixture size exceeded1MiB')
        target.chmod(0o600)
        receipt['samples'].append({'codec': codec, 'command': command, 'path': str(target), 'bytes': target.stat().st_size, 'sha256': bounded.sha256(target)})
    (output / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(output, flush=True)
    print(json.dumps(receipt, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', help='Reuse a verified private encoder stage')
    args = parser.parse_args()
    fixture(args.stage or prepare())


if __name__ == '__main__':
    os.umask(0o077)
    main()
