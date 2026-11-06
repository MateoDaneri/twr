#!/usr/bin/env python3
"""Observe synthetic raw pane output through a private tmux control client."""
import argparse
import json
import os
import re
import select
import subprocess
import tempfile
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record', action='store_true', help='record a synthetic escaped-output fixture')
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix='twr-yank-control-', dir='/private/tmp'))
    root.chmod(0o700)
    socket = root/'t.sock'
    env = {'HOME': str(root), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(root),
           'LANG': 'en_US.UTF-8', 'TERM': 'xterm-256color'}
    base = ['/opt/homebrew/bin/tmux', '-u', '-S', str(socket), '-f', '/dev/null']
    client = None

    def run(*args):
        return subprocess.check_output(base+list(args), env=env, cwd=root, stderr=subprocess.PIPE, timeout=8)

    def send(command):
        run('send-keys', '-t', 'fixture:', '-l', command)
        run('send-keys', '-t', 'fixture:', 'Enter')

    def read_until(needle, timeout=8):
        data = bytearray()
        end = time.monotonic()+timeout
        while time.monotonic() < end:
            if select.select([client.stdout], [], [], .1)[0]:
                chunk = os.read(client.stdout.fileno(), 65536)
                if not chunk:
                    break
                data.extend(chunk)
                if len(data) > 1024 * 1024:
                    raise RuntimeError('synthetic control output exceeded limit')
                if needle in data:
                    return bytes(data)
        raise RuntimeError('private control client timed out')

    try:
        run('new-session', '-d', '-s', 'fixture', '-x', '40', '-y', '10', '/bin/sh -i')
        send("printf 'TWR_BEFORE_%s\\n' CONTROL")
        deadline = time.monotonic()+8
        while b'TWR_BEFORE_CONTROL' not in run('capture-pane', '-p', '-t', 'fixture:'):
            if time.monotonic() > deadline:
                raise RuntimeError('private pane failed to start')
            time.sleep(.05)
        before_size = run('display-message', '-p', '-t', 'fixture:', '#{pane_width}x#{pane_height}').decode().strip()
        client = subprocess.Popen(['/opt/homebrew/bin/tmux', '-C', '-u', '-S', str(socket),
                                   'attach-session', '-t', 'fixture'], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=root)
        initial = read_until(b'%session-changed')
        after_size = run('display-message', '-p', '-t', 'fixture:', '#{pane_width}x#{pane_height}').decode().strip()
        send("printf '\\036written   \\nabcdef\\rxy\\033[K\\nabcdef\\rxy\\033[4X\\nTAB\\t\\303\\261\\347\\225\\214\\n\\037'")
        raw = read_until(b'\\037')
        encoded = b''.join(line.split(b' ', 2)[2] for line in raw.splitlines()
                           if line.startswith(b'%output ') and len(line.split(b' ', 2)) == 3)
        decoded = re.sub(rb'\\([0-7]{3})', lambda m: bytes([int(m.group(1), 8)]), encoded)
        payload = decoded.split(b'\x1e', 1)[1].split(b'\x1f', 1)[0]
        expected = b'written   \r\nabcdef\rxy\x1b[K\r\nabcdef\rxy\x1b[4X\r\nTAB\t\xc3\xb1\xe7\x95\x8c\r\n'
        if payload != expected or b'TWR_BEFORE_CONTROL' in initial+raw:
            raise RuntimeError('private control output differs from synthetic stream')
        encoded_payload = encoded.split(b'\\036', 1)[1].split(b'\\037', 1)[0]
        fixture = Path(__file__).resolve().parents[1]/'Resources/Fixtures/tmux-control-output.json'
        data = {'wire_hex': (b'%output %0 ' + encoded_payload + b'\n').hex(),
                'payload_hex': payload.hex()}
        if args.record:
            if fixture.is_symlink():
                raise RuntimeError('Refusing symlink fixture')
            fixture.write_text(json.dumps(data, separators=(',', ':')) + '\n')
        elif json.loads(fixture.read_text()) != data:
            raise RuntimeError('synthetic control fixture differs')
        print(json.dumps({'raw_payload_hex': payload.hex(), 'pre_attach_output_replayed': False,
                          'pane_size_before': before_size, 'pane_size_after': after_size,
                          'scope': 'synthetic private pane and control client only'}))
    finally:
        if socket.exists():
            subprocess.run(base+['kill-server'], env=env, cwd=root, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=8, check=False)
        if client:
            try:
                client.wait(timeout=3)
            except subprocess.TimeoutExpired:
                client.terminate()
                client.wait(timeout=3)


if __name__ == '__main__':
    main()
