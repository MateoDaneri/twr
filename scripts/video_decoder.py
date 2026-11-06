#!/usr/bin/env python3
"""Build an explicit private, pinned FFmpeg decoder. No global installation."""
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

VERSION = '9.0.2'
SOURCE_URL = 'https://ffmpeg.org/releases/ffmpeg-' + VERSION + '.tar.xz'
SOURCE_SHA = '8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e'
FLAGS = ['--disable-autodetect', '--disable-everything', '--enable-zlib',
         '--disable-doc', '--disable-network', '--disable-debug', '--disable-ffplay',
         '--enable-ffmpeg', '--enable-ffprobe', '--enable-protocol=file,pipe',
         '--enable-demuxer=mov,matroska,image2', '--enable-parser=h264,hevc,vp8,vp9,mpeg4video,png',
         '--enable-decoder=h264,hevc,vp8,vp9,mpeg4,png,mjpeg',
         '--enable-encoder=png', '--enable-muxer=image2', '--enable-filter=scale,fps,format']


def checked(supplied):
    stage = Path(supplied)
    info = stage.lstat()
    if stage.resolve(strict=True) != stage or stage.parent != Path('/private/tmp') or not stage.name.startswith('tw-video-decoder-') or not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('Expected an owned private tw-video-decoder-* stage')
    receipt_path = bounded.child(stage, stage / 'receipt.json')
    if receipt_path.stat().st_size > 1024**2:
        raise RuntimeError('Decoder receipt too large')
    receipt = json.loads(receipt_path.read_text())
    if receipt['state'] != 'verified' or receipt['version'] != VERSION or receipt['flags'] != FLAGS:
        raise RuntimeError('Decoder build not verified')
    for name in ['configure', 'build']:
        result = receipt[name]
        if result['returncode'] or result['stopped'] or result['cleanup_errors']:
            raise RuntimeError('Decoder supervision did not pass')
    archive = bounded.child(stage, stage / 'source.tar.xz')
    if archive.stat().st_size > 32 * 1024**2 or bounded.sha256(archive) != SOURCE_SHA:
        raise RuntimeError('Decoder source archive changed')
    for name, expected in receipt['binaries'].items():
        if name not in ['ffmpeg', 'ffprobe']:
            raise RuntimeError('Unexpected decoder binary')
        path = bounded.child(stage, stage / 'bin' / name)
        if path.stat().st_size != expected['bytes'] or bounded.sha256(path) != expected['sha256']:
            raise RuntimeError('Decoder binary changed')
    if set(receipt['binaries']) != {'ffmpeg', 'ffprobe'}:
        raise RuntimeError('Decoder binary missing')
    return stage, receipt


def main():
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise RuntimeError('Decoder build currently targets macOS arm64')
    for name in ['/usr/bin/clang', '/usr/bin/make', '/usr/bin/otool']:
        if not os.access(name, os.X_OK):
            raise RuntimeError('Required executable missing: ' + name)
    # The extracted source is capped at256MiB. Every download/build step also
    # stops at the shared2GiB free-space floor; partial artifacts are retained.
    if shutil.disk_usage('/private/tmp').free < 5 * 1024**3 // 2:
        raise RuntimeError('Build requires2.5GiB free; nothing deleted')
    stage = Path(tempfile.mkdtemp(prefix='tw-video-decoder-', dir='/private/tmp'))
    print(stage, flush=True)
    receipt = {'version': VERSION, 'flags': FLAGS, 'stage': str(stage), 'state': 'preparing',
               'driver_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    record = stage / 'receipt.json'
    record.write_text(json.dumps(receipt, indent=2) + '\n')
    receipt['source'] = bounded.get(SOURCE_URL, stage / 'source.tar.xz', 32 * 1024**2, SOURCE_SHA)
    receipt['extracted'] = bounded.extract(stage / 'source.tar.xz', stage / 'source',
                                           'ffmpeg-' + VERSION, 256 * 1024**2)
    record.write_text(json.dumps(receipt, indent=2) + '\n')
    commands = [('configure', ['/bin/sh', './configure', '--prefix=' + str(stage / 'install'), *FLAGS], 60),
                ('build', ['/usr/bin/make', '-j2', 'ffmpeg', 'ffprobe'], 480)]
    for name, command, seconds in commands:
        result = bounded.run_owned(stage, command, stage / (name + '.log'), timeout=seconds)
        receipt[name] = result
        record.write_text(json.dumps(receipt, indent=2) + '\n')
        if result['returncode'] or result['stopped'] or result['cleanup_errors']:
            raise RuntimeError('Decoder ' + name + ' failed; stage retained')
    output = stage / 'bin'
    output.mkdir(mode=0o700)
    receipt['binaries'] = {}
    for name in ['ffmpeg', 'ffprobe']:
        source = stage / 'source' / name
        destination = output / name
        if source.is_symlink() or not source.is_file() or source.stat().st_size > 32 * 1024**2:
            raise RuntimeError('Unexpected decoder output')
        shutil.copy2(source, destination)
        destination.chmod(0o700)
        options = {'env': bounded.environment(stage), 'cwd': stage, 'capture_output': True,
                   'timeout': 5, 'check': True, 'text': True}
        version = subprocess.run([str(destination), '-version'], **options).stdout
        protocols = subprocess.run([str(destination), '-protocols'], **options).stdout
        dependencies = subprocess.run(['/usr/bin/otool', '-L', str(destination)], **options).stdout
        if not version.startswith(name + ' version ' + VERSION) or '--disable-network' not in version:
            raise RuntimeError('Unexpected decoder build version/options')
        actual_protocols = {line.strip() for line in protocols.splitlines()
                            if line.strip() and not line.startswith('Supported') and not line.endswith(':')}
        if actual_protocols != {'file', 'pipe'}:
            raise RuntimeError('Unexpected decoder protocols')
        for line in dependencies.splitlines()[1:]:
            dependency = line.strip().split(' (')[0]
            if not dependency.startswith(('/usr/lib/', '/System/Library/')):
                raise RuntimeError('Unexpected non-system runtime library')
        receipt['binaries'][name] = {'bytes': destination.stat().st_size,
                                    'sha256': bounded.sha256(destination), 'version': version,
                                    'protocols': protocols, 'dependencies': dependencies}
    receipt['state'] = 'verified'
    record.write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps({'stage': str(stage), 'state': 'verified', 'receipt': str(record)}), flush=True)


if __name__ == '__main__':
    main()
