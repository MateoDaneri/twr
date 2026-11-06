#!/usr/bin/env python3
"""Create a deterministic PNG-in-MOV fixture. No capture, media reads or network.

--apple-h264/--apple-hevc use the native AVFoundation synthetic encoder.
Container fields: developer.apple.com/documentation/quicktime-file-format.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import struct
import tempfile
import zlib

ROOT = Path(__file__).resolve().parents[1]


def words(*values):
    return struct.pack('>' + 'I' * len(values), *values)


def atom(kind, data):
    return words(len(data) + 8) + kind.encode('ascii') + data


def png(frame):
    frame %= 24
    data = bytearray()
    for y in range(180):
        data.append(0)
        for x in range(320):
            square = frame * 11 <= x < frame * 11 + 48 and 45 <= y < 95
            progress = y >= 150 and x < (frame + 1) * 320 // 24
            data.extend((240, 55, 35) if square else (245, 210, 30) if progress else (20, 35 + y // 4, 70))
    def chunk(kind, payload):
        return words(len(payload)) + kind + payload + words(zlib.crc32(kind + payload))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', words(320, 180) + b'\x08\x02\0\0\0')
            + chunk(b'IDAT', zlib.compress(data, 9)) + chunk(b'IEND', b''))


def movie(count=24):
    frames = [png(n) for n in range(count)]
    matrix = words(65536, 0, 0, 0, 65536, 0, 0, 0, 0x40000000)
    ftyp = atom('ftyp', b'qt  ' + words(512) + b'qt  ')
    mdat = atom('mdat', b''.join(frames))
    mvhd = atom('mvhd', words(0, 0, 0, 12, count, 65536) + b'\x01\0' + bytes(10)
                + matrix + bytes(24) + words(2))
    tkhd = atom('tkhd', words(7, 0, 0, 1, 0, count) + bytes(16) + matrix + words(320 << 16, 180 << 16))
    mdhd = atom('mdhd', words(0, 0, 0, 12, count) + bytes(4))
    hdlr = atom('hdlr', words(0) + b'mhlrvide' + bytes(12) + b'\x09Synthetic')
    description = (bytes(6) + b'\0\x01' + bytes(16) + struct.pack('>HH', 320, 180)
                   + words(72 << 16, 72 << 16, 0) + b'\0\x01'
                   + b'\x0dSynthetic PNG' + bytes(18) + b'\0\x18\xff\xff')
    stbl = atom('stbl', atom('stsd', words(0, 1) + atom('png ', description))
                + atom('stts', words(0, 1, count, 1)) + atom('stsc', words(0, 1, 1, count, 1))
                + atom('stsz', words(0, 0, count, *(len(f) for f in frames)))
                + atom('stco', words(0, 1, len(ftyp) + 8)))
    dinf = atom('dinf', atom('dref', words(0, 1) + atom('url ', words(1))))
    minf = atom('minf', atom('vmhd', words(1) + bytes(8)) + dinf + stbl)
    trak = atom('trak', tkhd + atom('mdia', mdhd + hdlr + minf))
    return ftyp + mdat + atom('moov', mvhd + trak)


def apple_video(stage, receipt, codec):
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise RuntimeError('This native fixture targets macOS arm64')
    (stage / 'home').mkdir(mode=0o700)
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(stage / 'home'), 'TMPDIR': str(stage)}
    sdk = subprocess.run(['/usr/bin/xcrun', '--sdk', 'macosx', '--show-sdk-path'],
                         env=env, capture_output=True, check=True, timeout=5).stdout.decode().strip()
    source = ROOT / 'scripts/fixtures/video_writer.m'
    command = ['/usr/bin/clang', '-fobjc-arc', '-fblocks', '-isysroot', sdk,
               str(source), '-framework', 'AVFoundation', '-framework', 'CoreVideo',
               '-framework', 'CoreMedia', '-framework', 'Foundation', '-o', str(stage / 'writer')]
    receipt.update(writer_source_sha256=hashlib.sha256(source.read_bytes()).hexdigest(), compile=command)
    for name, cmd in [('compile', command), ('generate', [str(stage / 'writer'), str(stage / 'synthetic.mp4'), codec])]:
        try:
            with (stage / (name + '.log')).open('wb') as log:
                result = subprocess.run(cmd, env=env, stdin=subprocess.DEVNULL, stdout=log,
                                        stderr=subprocess.STDOUT, timeout=40)
        except subprocess.TimeoutExpired:
            receipt[name + '_timeout'] = 40
            (stage / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
            raise
        receipt[name + '_exit'] = result.returncode
        (stage / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
        if result.returncode:
            raise RuntimeError(name + ' failed; artifacts retained at ' + str(stage))
    return stage / 'synthetic.mp4'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    native = parser.add_mutually_exclusive_group()
    native.add_argument('--apple-h264', action='store_true')
    native.add_argument('--apple-hevc', action='store_true')
    parser.add_argument('--seconds', type=int, choices=[2, 8], default=2)
    args = parser.parse_args()
    codec = 'hevc' if args.apple_hevc else 'h264' if args.apple_h264 else 'png'
    if codec != 'png' and args.seconds != 2:
        parser.error('Apple codec fixtures support only2seconds')
    stage = Path(tempfile.mkdtemp(prefix='tw-video-fixture-', dir='/private/tmp'))
    print(stage, flush=True)
    receipt = {'scope': 'Only synthetic pixels; no desktop capture, user media, audio, or network',
               'stage': str(stage), 'source_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
               'codec': codec, 'width': 320, 'height': 180,
               'frames': args.seconds * 12, 'fps': 12, 'duration_seconds': args.seconds}
    if codec != 'png':
        output = apple_video(stage, receipt, codec)
    else:
        output = stage / 'synthetic.mov'
        with output.open('xb') as stream:
            stream.write(movie(args.seconds * 12))
    os.chmod(output, 0o600)
    if output.stat().st_size > 1024 * 1024:
        raise RuntimeError('Synthetic fixture exceeded 1 MiB; artifacts retained')
    receipt['output'] = str(output)
    receipt['bytes'] = output.stat().st_size
    receipt['sha256'] = hashlib.sha256(output.read_bytes()).hexdigest()
    (stage / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
