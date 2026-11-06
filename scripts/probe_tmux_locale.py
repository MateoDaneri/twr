#!/usr/bin/env python3
"""Reproduce tmux format output under C/UTF-8, using four private servers only."""
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path


def main():
    os.umask(0o077)
    stage = Path(tempfile.mkdtemp(prefix='tw-tmux-locale-reference-', dir='/private/tmp'))
    binary = Path('/opt/homebrew/bin/tmux')
    if not binary.is_file():
        raise RuntimeError('Expected the existing /opt/homebrew/bin/tmux')
    root = Path(__file__).resolve().parents[1]
    config = stage/'navigation.conf'
    config.write_bytes((root/'Resources/Fixtures/native-tmux.conf').read_bytes())
    deadline = time.monotonic()+25
    results = []
    for index, (locale, utf8) in enumerate([
        ('C', False), ('C', True), ('en_US.UTF-8', False), ('en_US.UTF-8', True),
    ]):
        socket = stage/(str(index)+'.sock')
        env = {'HOME': str(stage), 'TMPDIR': str(stage), 'PATH': '/usr/bin:/bin',
               'LC_ALL': locale, 'LANG': 'en_US.UTF-8', 'COLORTERM': 'truecolor'}
        command = [str(binary)]+(['-u'] if utf8 else [])+['-S', str(socket)]

        def run(arguments, cleanup=False):
            if not cleanup and time.monotonic() >= deadline:
                raise TimeoutError('tmux locale probe overall deadline')
            output = subprocess.run(command+arguments, env=env, cwd=stage,
                                    capture_output=True, timeout=3)
            return {'status': output.returncode,
                    'stdout': output.stdout.decode('utf-8', errors='replace'),
                    'stderr': output.stderr.decode('utf-8', errors='replace')}

        try:
            start = run(['-f', str(config), 'new-session', '-d', '-s', 'tw_probe',
                         '-n', 'terminal-ñ', '-c', str(stage), '/bin/sh -i'])
            windows = run(['list-windows', '-t', 'tw_probe', '-F',
                           '#{window_id}\t#{window_index}\t#{window_active}\t#{window_name}'])
            target = run(['display-message', '-p', '-t', 'tw_probe:',
                          '#{window_id}\t#{pane_id}'])
            server_locale = run(['show-environment', '-g', 'LC_ALL'])
            results.append({'locale': locale, 'utf8_flag': utf8,
                            'server_locale': server_locale,
                            'start': start, 'windows': windows, 'target': target})
        finally:
            # Never use the default tmux socket, user sessions or shell commands.
            run(['kill-server'], cleanup=True)
    version = subprocess.check_output([str(binary), '-V'], cwd=stage,
                                      env={'PATH': '/usr/bin:/bin'}, timeout=3).decode().strip()
    data = {'version': version, 'scope': 'four synthetic private servers, no app GUI',
            'results': results}
    path = stage/'results.json'
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2)+'\n')
    for item in results:
        assert item['start']['status'] == 0, path
        assert item['server_locale']['status'] == 0, path
        assert item['server_locale']['stdout'] == 'LC_ALL='+item['locale']+'\n', path
        assert item['windows']['status'] == item['target']['status'] == 0, path
        if item['locale'] != 'C' or item['utf8_flag']:
            assert item['windows']['stdout'] == '@0\t1\t1\tterminal-ñ\n', path
            assert item['target']['stdout'] == '@0\t%0\n', path
        else:
            assert '\t' not in item['windows']['stdout'], path
            assert '\t' not in item['target']['stdout'], path
    print(json.dumps({'result': 'PASS', 'evidence': str(path), 'version': version}))


if __name__ == '__main__':
    main()
