#!/usr/bin/env python3
"""Offline file guards, or native isolated HTML controls with owned fixtures only.

The --native controls run from the synthetic app terminal, like test_tex_sandbox.
They never request a public URL or read personal files.
"""
import argparse
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import stat
import struct
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch
import urllib.request
import zlib

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('renderer', ROOT / 'Resources/Helpers/render_preview.py')
renderer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(renderer)


class FileGuards(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='tw-html-guards-', dir='/private/tmp')
        self.stage = Path(self.temporary.name)
        self.project, self.output = self.stage / 'project', self.stage / 'output'
        self.project.mkdir(mode=0o700)
        self.output.mkdir(mode=0o700)
        self.source = self.project / 'example.html'
        self.source.write_text('<h1>Datos sintéticos ñ 🦀</h1>')

    def tearDown(self):
        self.temporary.cleanup()

    def test_exact_private_copy_preserves_original(self):
        original = self.source.read_bytes()
        copied = renderer.copy_html(self.source, self.project, self.output)
        self.assertEqual(copied.read_bytes(), original)
        self.assertEqual(self.source.read_bytes(), original)
        self.assertEqual(copied.stat().st_mode & 0o777, 0o600)

    def test_outside_and_symlink_rejected(self):
        outside = self.stage / 'outside.html'
        outside.write_text('unrelated synthetic fixture')
        for source in [outside, self.project / 'linked.html']:
            if source != outside:
                source.symlink_to(outside)
            with self.assertRaises(RuntimeError):
                renderer.copy_html(source, self.project, self.output)
        self.assertFalse((self.output / 'document.html').exists())

    def test_oversize_and_fifo_do_not_enter_a_reader(self):
        self.source.write_bytes(b'')
        with self.source.open('wb') as stream:
            stream.truncate(2 * 1024**2 + 1)
        with self.assertRaises(RuntimeError):
            renderer.copy_html(self.source, self.project, self.output)
        fifo = self.project / 'fifo.html'
        os.mkfifo(fifo, 0o600)
        start = time.monotonic()
        with self.assertRaises(RuntimeError):
            renderer.copy_html(fifo, self.project, self.output)
        self.assertLess(time.monotonic() - start, 1)

    def test_invalid_encoding_and_public_output_rejected(self):
        self.source.write_bytes(b'\xff\xfe\xff')
        with self.assertRaises(RuntimeError):
            renderer.copy_html(self.source, self.project, self.output)
        self.source.write_text('valid UTF-8')
        self.output.chmod(0o755)
        with self.assertRaises(RuntimeError):
            renderer.copy_html(self.source, self.project, self.output)

    def test_existing_copy_is_preserved(self):
        copied = self.output / 'document.html'
        copied.write_text('preserve this generated test fixture')
        with self.assertRaises(FileExistsError):
            renderer.copy_html(self.source, self.project, self.output)
        self.assertEqual(copied.read_text(), 'preserve this generated test fixture')

    def test_private_source_copy_removed_after_success_or_failure(self):
        binary = self.stage / 'bundle/Contents/MacOS/helper'
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b'synthetic placeholder; never executed')
        binary.chmod(0o700)
        for failed in [False, True]:
            def child(*_args, **_kwargs):
                self.assertEqual((self.output / 'document.html').read_bytes(), self.source.read_bytes())
                if failed:
                    raise RuntimeError('synthetic child failure')
            with patch.object(renderer, 'run', child):
                if failed:
                    with self.assertRaises(RuntimeError):
                        renderer.render_html(self.source, self.project, self.output, binary)
                else:
                    renderer.render_html(self.source, self.project, self.output, binary)
            self.assertFalse((self.output / 'document.html').exists())
            self.assertTrue(self.source.is_file())


def png(color):
    def chunk(name, content):
        return struct.pack('>I', len(content)) + name + content + struct.pack('>I', zlib.crc32(name + content))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(bytes([0, *color]))) + chunk(b'IEND', b''))


def sample_pixels(path, positions):
    """Read bounded 8-bit RGB/RGBA PNG pixels; no image modification."""
    data = path.read_bytes()
    assert len(data) <= 16 * 1024**2 and data[:8] == b'\x89PNG\r\n\x1a\n'
    offset, compressed = 8, bytearray()
    width = height = channels = 0
    while offset + 12 <= len(data):
        size = struct.unpack_from('>I', data, offset)[0]
        kind, body = data[offset + 4:offset + 8], data[offset + 8:offset + 8 + size]
        assert offset + size + 12 <= len(data)
        if kind == b'IHDR':
            width, height, depth, color, compression, filtering, interlace = struct.unpack('>IIBBBBB', body)
            assert depth == 8 and color in (2, 6) and not compression and not filtering and not interlace
            assert 0 < width <= 3000 and 0 < height <= 4400
            channels = 3 if color == 2 else 4
        elif kind == b'IDAT':
            compressed.extend(body)
        offset += size + 12
    stride = width * channels
    expected = (stride + 1) * height
    decoder = zlib.decompressobj()
    raw = decoder.decompress(compressed, expected + 1)
    assert decoder.eof and len(raw) == expected
    scaled = {position: (position[0] * width // 1500, position[1] * width // 1500) for position in positions}
    previous, result = bytearray(stride), {}
    for y in range(max(position[1] for position in scaled.values()) + 1):
        start = y * (stride + 1)
        filter_type = raw[start]
        row = bytearray(raw[start + 1:start + 1 + stride])
        assert filter_type in range(5)
        for i in range(stride):
            left = row[i - channels] if i >= channels else 0
            up = previous[i]
            corner = previous[i - channels] if i >= channels else 0
            if filter_type == 1:
                predictor = left
            elif filter_type == 2:
                predictor = up
            elif filter_type == 3:
                predictor = (left + up) // 2
            elif filter_type == 4:
                p = left + up - corner
                a, b, c = abs(p - left), abs(p - up), abs(p - corner)
                predictor = left if a <= b and a <= c else up if b <= c else corner
            else:
                predictor = 0
            row[i] = (row[i] + predictor) & 255
        for original, (x, row_y) in scaled.items():
            if row_y == y:
                result[original] = tuple(row[x * channels:x * channels + 3])
        previous = row
    return result


def native(binary):
    binary = binary.resolve(strict=True)
    stage = Path(tempfile.mkdtemp(prefix='tw-html-native-controls-', dir='/private/tmp'))
    project, forbidden, output = [stage / name for name in ['project', 'forbidden', 'output']]
    for directory in [project, forbidden, output]:
        directory.mkdir(mode=0o700)
    (forbidden / 'canary.png').write_bytes(png((255, 0, 0)))
    (forbidden / 'canary.txt').write_text('UNRELATED_SYNTHETIC_CANARY\n')
    connections, requests = [], []
    red = png((255, 0, 0))

    class Server(ThreadingHTTPServer):
        def get_request(self):
            item = super().get_request()
            connections.append(True)
            return item

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(self.path)
            self.send_response(200)
            self.send_header('Content-Type', 'image/png')
            self.send_header('Content-Length', str(len(red)))
            self.end_headers()
            self.wfile.write(red)

        def log_message(self, *_args):
            pass

    server = Server(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    checks = []
    report = {'stage': str(stage), 'binary': str(binary), 'synthetic_only': True, 'checks': checks}
    try:
        address = 'http://127.0.0.1:' + str(server.server_port)
        with urllib.request.urlopen(address + '/control.png', timeout=3) as response:
            assert response.read(1024) == red
        assert len(connections) == 1 and len(requests) == 1
        checks.append({'name': 'loopback-listener-control', 'passed': True})
        connections.clear()
        requests.clear()
        green = base64.b64encode(png((0, 255, 0))).decode('ascii')
        source = project / 'controls.html'
        source.write_text(f'''<!doctype html><meta charset="utf-8">
<link rel="preconnect" href="{address}">
<link rel="dns-prefetch" href="//127.0.0.1">
<style>body{{margin:0;background:rgb(0,0,255);color:white;font:24px system-ui}}.box{{position:absolute;top:0;width:80px;height:80px}}</style>
<div id="script-guard" class="box" style="left:0;background:rgb(0,255,0)"></div>
<img class="box" style="left:100px" src="data:image/png;base64,{green}">
<img class="box" style="left:200px" src="{(forbidden / 'canary.png').as_uri()}">
<img class="box" style="left:300px" src="{address}/blocked.png">
<svg class="box" style="left:400px" viewBox="0 0 80 80"><rect width="80" height="80" fill="rgb(128,0,255)"/></svg>
<p style="position:absolute;top:100px">HTML_LOCAL_STATIC_FIXTURE · Unicode ñ 🦀</p>
<script>document.getElementById('script-guard').style.background='rgb(255,0,0)';fetch('{address}/script');</script>''')
        before = hashlib.sha256(source.read_bytes()).hexdigest()
        renderer.render_html(source, project, output, binary, redact_errors=False)
        positions = [(40, 40), (140, 40), (240, 40), (340, 40), (440, 40)]
        colors = sample_pixels(output / 'preview.png', positions)
        expected = [(0, 255, 0), (0, 255, 0), (0, 0, 255), (0, 0, 255), (128, 0, 255)]
        for name, position, color in zip(['scripts-disabled', 'data-image-control', 'outside-file-image-blocked', 'network-image-blocked', 'inline-svg-control'], positions, expected):
            actual = colors[position]
            checks.append({'name': name, 'expected_rgb': color, 'actual_rgb': actual, 'passed': actual == color})
            assert actual == color, checks[-1]
        time.sleep(.3)
        checks.append({'name': 'no-renderer-loopback-connections', 'connections': len(connections), 'requests': requests[:8], 'passed': not connections and not requests})
        assert not connections and not requests, checks[-1]
        assert hashlib.sha256(source.read_bytes()).hexdigest() == before
        checks.append({'name': 'original-source-unchanged', 'sha256': before, 'passed': True})
        assert not (output / 'document.html').exists()
        checks.append({'name': 'private-html-copy-removed', 'passed': True})
        (output / 'fixture-control.txt').write_text('SYNTHETIC_ALLOWED_CONTROL\n')
        profile = renderer.sandbox_profile(output, binary.parent.parent)
        env = {'PATH': '/usr/bin:/bin', 'HOME': str(output), 'TMPDIR': str(output), 'LANG': 'en_US.UTF-8'}
        for name, command, permitted in [
            ('read-private-job-control', ['/bin/cat', str(output / 'fixture-control.txt')], True),
            ('deny-unrelated-file', ['/bin/cat', str(forbidden / 'canary.txt')], False),
            ('deny-project-write', ['/bin/sh', '-c', 'printf denied > "$1"', 'probe', str(project / 'blocked-write')], False),
            ('deny-loopback-socket', ['/usr/bin/nc', '-v', '-z', '-w', '1', '127.0.0.1', str(server.server_port)], False),
        ]:
            result = subprocess.run(['/usr/bin/sandbox-exec', '-p', profile, *command], cwd=output, env=env,
                                    stdin=subprocess.DEVNULL, capture_output=True, timeout=5)
            passed = result.returncode == (0 if permitted else 1)
            if not permitted:
                passed = passed and b'Operation not permitted' in result.stderr
            checks.append({'name': name, 'returncode': result.returncode, 'passed': passed})
            assert passed, checks[-1]
        assert not (project / 'blocked-write').exists()
        report.update(passed=True, png=str(output / 'preview.png'), png_sha256=hashlib.sha256((output / 'preview.png').read_bytes()).hexdigest())
    except Exception as error:
        report.update(passed=False, error=str(error))
        raise
    finally:
        server.shutdown()
        server.server_close()
        (stage / 'receipt.json').write_text(json.dumps(report, indent=2) + '\n')
        print(stage, flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native', type=Path, help='Signed helper from an owned build; run in synthetic app terminal')
    args = parser.parse_args()
    if args.native:
        native(args.native)
    else:
        unittest.main(argv=[__file__])
