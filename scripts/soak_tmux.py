#!/usr/bin/env python3
"""Bounded synthetic tmux transport soak. No GUI, personal shell or user sessions.
Writes only aggregate evidence into a new private temporary directory.
Stop gracefully by creating STOP in the printed output directory.
"""
import argparse
import datetime
import fcntl
import json
import os
from pathlib import Path
import pty
import resource
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--seconds', type=int, default=8 * 3600)
    parser.add_argument('--interval', type=int, default=60)
    args = parser.parse_args()
    if not 10 <= args.seconds <= 24 * 3600 or not 1 <= args.interval <= 60:
        parser.error('seconds must be 10..86400; interval must be 1..60')
    binary = shutil.which('tmux', path='/opt/homebrew/bin:/usr/local/bin:/usr/bin')
    if not binary:
        raise RuntimeError('tmux is not installed; nothing is installed automatically')
    run = Path(tempfile.mkdtemp(prefix='tw-transport-soak-', dir='/private/tmp'))
    run.chmod(0o700)
    env = {'HOME': str(run), 'USER': 'synthetic', 'PATH': '/usr/bin:/bin:/opt/homebrew/bin',
           'TERM': 'xterm-256color', 'LANG': 'en_US.UTF-8', 'TMPDIR': str(run)}
    socket = run / 'tmux.sock'
    target = 'soak:1.1'
    base = [binary, '-S', str(socket)]
    cancelled = False
    def stop(_signum, _frame):
        nonlocal cancelled
        cancelled = True
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    def command(*arguments):
        return subprocess.check_output([*base, *arguments], env=env, cwd=run,
                                       stderr=subprocess.PIPE, timeout=5).decode('utf-8', 'replace')
    def send(text):
        command('send-keys', '-t', target, '-l', text)
        command('send-keys', '-t', target, 'Enter')
    def value(format_text):
        return command('display-message', '-p', '-t', target, format_text).strip()
    def capture():
        return command('capture-pane', '-p', '-t', target, '-S', '-100')
    started = time.monotonic()
    report = {'schema': 1, 'scope': 'synthetic private tmux transport only; no GPUI renderer, personal shell, providers or 24/7 certification',
              'pid': os.getpid(), 'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'tmux': binary, 'requested_seconds': args.seconds, 'interval_seconds': args.interval,
              'directory': str(run), 'status': 'starting', 'iterations': 0, 'reattachments': 0,
              'completed_seconds': 0, 'failures': []}
    def save():
        report['completed_seconds'] = round(time.monotonic() - started, 2)
        temp = run / 'report.tmp'
        temp.write_text(json.dumps(report, indent=2) + '\n')
        temp.chmod(0o600)
        temp.replace(run / 'report.json')
    print(json.dumps({'directory': str(run), 'report': str(run / 'report.json'), 'pid': os.getpid()}), flush=True)
    client = None
    master = None
    try:
        command('-f', str(ROOT / 'Resources/Fixtures/native-tmux.conf'), 'new-session', '-d',
                '-s', 'soak', '-x', '100', '-y', '30', '-c', str(run), '/bin/sh -i')
        pane_pid = value('#{pane_pid}')
        report['pane_pid'] = int(pane_pid)
        seed = run.name.rsplit('-', 1)[-1]
        send('TW_SOAK_SEED=' + seed)
        report['status'] = 'running'
        save()
        while not cancelled and not (run / 'STOP').exists() and time.monotonic() - started < args.seconds:
            tick = report['iterations'] + 1
            width, height = (100, 30) if tick % 2 else (82, 24)
            command('resize-window', '-t', 'soak:1', '-x', str(width), '-y', str(height))
            marker = 'SOAK-%s:%s:%s' % (tick, pane_pid, seed)
            # Prompt input contains format placeholders, so only executed output can match marker.
            send("printf 'SOAK-%s:%s:%s\\n' '" + str(tick) + "' \"$$\" \"$TW_SOAK_SEED\"")
            deadline = time.monotonic() + 5
            while marker not in capture():
                if time.monotonic() > deadline:
                    raise RuntimeError('Missing synthetic marker or shell state lost at iteration ' + str(tick))
                time.sleep(0.02)
            if value('#{pane_pid}') != pane_pid or value('#{pane_dead}') != '0':
                raise RuntimeError('Synthetic pane process changed or exited')
            client, master = pty.fork()
            if client == 0:
                os.chdir(run)
                os.execve(binary, [*base, 'attach-session', '-t', 'soak'], env)
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', height, width, 0, 0))
            deadline = time.monotonic() + 1
            received = 0
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        break
                    if not data:
                        break
                    received += len(data)
                    if received > 2 * 1024 * 1024:
                        raise RuntimeError('Synthetic client output exceeded 2 MiB bound')
            if received == 0:
                raise RuntimeError('Reattached tmux client produced no output')
            os.kill(client, signal.SIGTERM)
            os.waitpid(client, 0)
            client = None
            os.close(master)
            master = None
            if value('#{pane_pid}') != pane_pid:
                raise RuntimeError('Dropping synthetic client changed shell PID')
            report['iterations'] = tick
            report['reattachments'] += 1
            report['peak_driver_rss_bytes_macos'] = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
            save()
            with (run / 'samples.jsonl').open('a') as stream:
                stream.write(json.dumps({'iteration': tick, 'elapsed_seconds': report['completed_seconds'],
                                         'width': width, 'height': height, 'client_bytes': received,
                                         'pane_pid_preserved': True}) + '\n')
            until = min(started + args.seconds, time.monotonic() + args.interval)
            while time.monotonic() < until and not cancelled and not (run / 'STOP').exists():
                time.sleep(max(0, min(1, until - time.monotonic())))
        report['status'] = 'stopped' if cancelled or (run / 'STOP').exists() else 'passed'
    except Exception as error:
        report['status'] = 'failed'
        report['failures'].append(str(error))
        raise
    finally:
        if client:
            try:
                os.kill(client, signal.SIGTERM)
                os.waitpid(client, 0)
            except ProcessLookupError:
                pass
        if master is not None:
            os.close(master)
        try:
            command('kill-server')
            report['private_server_cleanup'] = True
        except subprocess.CalledProcessError:
            report['private_server_cleanup'] = not socket.exists()
        save()
        print(json.dumps({'status': report['status'], 'iterations': report['iterations'],
                          'completed_seconds': report['completed_seconds'], 'report': str(run / 'report.json')}), flush=True)
    if report['status'] == 'failed':
        raise SystemExit(1)


if __name__ == '__main__':
    main()
