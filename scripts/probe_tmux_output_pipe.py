#!/usr/bin/env python3
"""Inspect raw output of a private synthetic tmux pane; never attach to user sessions."""
import json
import subprocess
import tempfile
import time
from pathlib import Path


def main():
    root = Path(tempfile.mkdtemp(prefix='twr-yank-pipe-', dir='/private/tmp'))
    root.chmod(0o700)
    socket = root/'t.sock'
    stream = root/'stream'
    env = {'HOME': str(root), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(root), 'LANG': 'en_US.UTF-8',
           'TERM': 'xterm-256color'}
    tmux = ['/opt/homebrew/bin/tmux', '-u', '-S', str(socket), '-f', '/dev/null']

    def run(*args):
        return subprocess.check_output(tmux+list(args), env=env, cwd=root, stderr=subprocess.PIPE, timeout=8)

    def wait_for(read, predicate):
        deadline = time.monotonic()+8
        while time.monotonic() < deadline:
            value = read()
            if predicate(value):
                return value
            time.sleep(.05)
        raise RuntimeError('private tmux fixture timed out')

    def send(command):
        run('send-keys', '-t', 'fixture:', '-l', command)
        run('send-keys', '-t', 'fixture:', 'Enter')

    try:
        run('new-session', '-d', '-s', 'fixture', '-x', '40', '-y', '10', '/bin/sh -i')
        send("printf 'TWR_BEFORE_%s\\n' PIPE")
        wait_for(lambda: run('capture-pane', '-p', '-t', 'fixture:'),
                 lambda b: b'TWR_BEFORE_PIPE' in b)
        # -o avoids replacing any existing pipe; this pane is synthetic and private.
        run('pipe-pane', '-o', '-O', '-t', 'fixture:', 'umask 077; exec /bin/cat > '+str(stream))
        send("printf '\\036written   \\nabcdef\\rxy\\033[K\\nabcdef\\rxy\\033[4X\\nTAB\\t\\303\\261\\347\\225\\214\\n\\037'")
        raw = wait_for(lambda: stream.read_bytes() if stream.exists() else b'', lambda b: b'\x1f' in b)
        payload = raw.split(b'\x1e', 1)[1].split(b'\x1f', 1)[0]
        expected = b'written   \r\nabcdef\rxy\x1b[K\r\nabcdef\rxy\x1b[4X\r\nTAB\t\xc3\xb1\xe7\x95\x8c\r\n'
        if payload != expected or b'TWR_BEFORE_PIPE' in raw:
            raise RuntimeError('private output pipe differs from expected synthetic stream')
        print(json.dumps({'raw_payload_hex': payload.hex(), 'pre_attach_output_replayed': False,
                          'scope': 'synthetic private pane only'}))
    finally:
        if socket.exists():
            subprocess.run(tmux+['kill-server'], env=env, cwd=root, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=8, check=False)


if __name__ == '__main__':
    main()
