#!/usr/bin/env python3
"""Decode one explicit local video in a private job directory. No network/auth."""
import json
import math
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import time

MAX_INPUT = 100 * 1024**2
MAX_CACHE = 256 * 1024**2
PARENT = int(os.environ.get('TW_VIDEO_PARENT_PID', os.getppid()))


def parent_alive():
    if os.getppid() != PARENT:
        raise RuntimeError('Video: la app se cerró durante la preparación')


def regular(path, limit):
    info = path.lstat()
    if path.resolve(strict=True) != path or not stat.S_ISREG(info.st_mode) or info.st_size > limit:
        raise RuntimeError('Video: archivo fuera de límites o symlink')
    return info


def private(path):
    info = path.lstat()
    if path.resolve(strict=True) != path or not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('Video: directorio privado inválido')


def cache_bytes(entries):
    total = 0
    for path in entries:
        try:
            info = path.lstat()
        except FileNotFoundError:
            # image2 atomic_writing renames *.png.tmp between directory listing
            # and stat. The next poll sees the final PNG; final validation runs
            # after the writer exits and still enforces the full byte budget.
            continue
        if not stat.S_ISREG(info.st_mode):
            raise RuntimeError('Video: salida inesperada del decoder')
        total += info.st_size
    return total


def publish(path, data):
    temporary = path.with_suffix('.json.tmp')
    with temporary.open('w') as stream:
        json.dump(data, stream)
        stream.write('\n')
    temporary.replace(path)


def publish_progress(frames, metadata, previous):
    # Only complete atomic-renamed PNGs form the published contiguous prefix.
    # A *.tmp file is never visible to the player.
    count, size = 0, 0
    for index in range(1, 2161):
        path = frames / f'frame-{index:06}.png'
        try:
            info = regular(path, 8 * 1024**2)
        except FileNotFoundError:
            break
        count += 1
        size += info.st_size
    if size > MAX_CACHE:
        raise RuntimeError('Video: cache de cuadros supera256MiB')
    if count and count != previous:
        publish(frames.parent / 'progress.json', {**metadata, 'frames': count, 'cache_bytes': size})
    return count


def run(command, output, error, seconds, env, frames=None, metadata=None):
    with output.open('xb') as stdout, error.open('xb') as stderr:
        process = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                                   stdout=stdout, stderr=stderr)
        started = time.monotonic()
        published = 0
        try:
            while process.poll() is None:
                parent_alive()
                if time.monotonic() - started > seconds:
                    raise RuntimeError('Video: decodificación fuera de tiempo')
                if output.stat().st_size > 65536 or error.stat().st_size > 65536:
                    raise RuntimeError('Video: salida del decoder fuera de límites')
                if frames is not None:
                    entries = list(frames.iterdir())
                    if len(entries) > 2161 or cache_bytes(entries) > MAX_CACHE:
                        raise RuntimeError('Video: cache de cuadros supera256MiB')
                    if metadata is not None:
                        published = publish_progress(frames, metadata, published)
                if shutil.disk_usage(output.parent).free < 64 * 1024**2:
                    raise RuntimeError('Video: menos de64MiB libres')
                time.sleep(0.02)
            if process.returncode:
                # Decoder stderr may contain file names or media metadata.
                raise RuntimeError('Video: el decoder rechazó el archivo')
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)


def main():
    if len(sys.argv) != 5:
        raise RuntimeError('Expected source, project root, private job, decoder directory')
    source, root, job, decoder = (Path(x) for x in sys.argv[1:])
    parent_alive()
    private(job)
    root = root.resolve(strict=True)
    info = regular(source, MAX_INPUT)
    if not source.is_relative_to(root) or source.suffix.lower() not in ('.mp4', '.mov', '.webm'):
        raise RuntimeError('Video: seleccioná MP4/MOV/WebM dentro del proyecto')
    for name in ['ffprobe', 'ffmpeg']:
        regular(decoder / name, 32 * 1024**2)
        if not os.access(decoder / name, os.X_OK):
            raise RuntimeError('Video: decoder no ejecutable')
    (job / 'home').mkdir(mode=0o700)
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(job / 'home'), 'TMPDIR': str(job),
           'LC_ALL': 'C', 'AV_LOG_FORCE_NOCOLOR': '1'}
    copy = job / ('input' + source.suffix.lower())
    fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream, copy.open('xb') as output:
        if (os.fstat(stream.fileno()).st_dev, os.fstat(stream.fileno()).st_ino) != (info.st_dev, info.st_ino):
            raise RuntimeError('Video: el archivo cambió antes de abrir')
        count = 0
        while chunk := stream.read(65536):
            parent_alive()
            count += len(chunk)
            if count > MAX_INPUT:
                raise RuntimeError('Video: archivo supera100MiB')
            if shutil.disk_usage(job).free < 64 * 1024**2:
                raise RuntimeError('Video: menos de64MiB libres')
            output.write(chunk)
        after = os.fstat(stream.fileno())
        if count != info.st_size or after.st_size != info.st_size or after.st_mtime_ns != info.st_mtime_ns:
            raise RuntimeError('Video: el archivo cambió durante la copia')
    copy.chmod(0o600)
    common = ['-v', 'error', '-max_alloc', str(32 * 1024**2), '-protocol_whitelist', 'file,pipe',
              '-format_whitelist', 'mov,matroska,webm', '-probesize', str(5 * 1024**2),
              '-analyzeduration', '5000000']
    run([str(decoder / 'ffprobe'), *common, '-select_streams', 'v:0', '-show_entries',
         'stream=width,height:format=duration', '-of', 'json', str(copy)],
        job / 'probe.json', job / 'probe.log', 15, env)
    if (job / 'probe.json').stat().st_size > 65536:
        raise RuntimeError('Video: metadata fuera de límites')
    probe = json.loads((job / 'probe.json').read_text())
    streams = probe.get('streams', [])
    duration = float(probe.get('format', {}).get('duration', 0))
    if len(streams) != 1 or not math.isfinite(duration) or not 0 < duration <= 180:
        raise RuntimeError('Video: duración inválida o supera180segundos')
    width, height = (int(streams[0].get(key, 0)) for key in ['width', 'height'])
    if not 0 < width <= 4096 or not 0 < height <= 4096:
        raise RuntimeError('Video: dimensiones inválidas o superan4096')
    frames = job / 'frames'
    frames.mkdir(mode=0o700)
    metadata = {'schema': 1, 'fps': 12, 'duration': duration,
                'source_width': width, 'source_height': height}
    run([str(decoder / 'ffmpeg'), '-nostdin', '-hide_banner', *common, '-threads', '2',
         '-i', str(copy), '-t', str(duration), '-map', '0:v:0', '-an', '-sn', '-dn',
         '-vf', "fps=12,scale='min(1280,iw)':'min(1280,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
         '-frames:v', str(math.ceil(duration * 12)), '-threads', '2', '-filter_threads', '1',
         '-compression_level', '1', '-atomic_writing', '1', str(frames / 'frame-%06d.png')],
        job / 'decode.out', job / 'decode.log', 120, env, frames, metadata)
    entries = sorted(frames.iterdir())
    if not entries or len(entries) > 2160:
        raise RuntimeError('Video: sin cuadros o cantidad fuera de límites')
    size = 0
    for index, path in enumerate(entries, 1):
        if path.name != f'frame-{index:06}.png':
            raise RuntimeError('Video: salida inesperada del decoder')
        size += regular(path, 8 * 1024**2).st_size
        path.chmod(0o600)
    if size > MAX_CACHE:
        raise RuntimeError('Video: cache de cuadros supera256MiB')
    manifest = {'schema': 1, 'fps': 12, 'frames': len(entries), 'duration': duration,
                'source_width': width, 'source_height': height, 'cache_bytes': size}
    publish(job / 'manifest.json', manifest)
    print(json.dumps(manifest))


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except Exception as error:
        # No traceback/file names, URL queries or raw decoder metadata in UI.
        print(str(error) if isinstance(error, RuntimeError) else 'Video: no se pudo preparar el archivo', file=sys.stderr)
        sys.exit(1)
