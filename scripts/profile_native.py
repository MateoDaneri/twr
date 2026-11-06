#!/usr/bin/env python3
"""Bounded, private Time Profiler capture/export for a signed synthetic TWR candidate."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import resource
import shutil
import signal
import stat
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
MAX_BYTES = 128 * 1024 * 1024
MIN_FREE = 2 * 1024 ** 3


def no_links(path):
    path = Path(path).absolute()
    for parent in [*reversed(path.parents), path]:
        if parent.is_symlink():
            raise ValueError('Symlink path refused')
    return path


def private_dir(path):
    path = no_links(path)
    info = path.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError('Output must be an owned private directory')
    return path


def exclusive(path, content):
    fd = os.open(no_links(path), os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as stream:
        stream.write(content)


def size_under(path):
    total = 0
    entries = 0
    for base, dirs, files in os.walk(path, followlinks=False):
        for name in dirs + files:
            entries += 1
            if entries > 4096:
                raise ValueError('Too many output entries')
            entry = Path(base) / name
            info = entry.lstat()
            if stat.S_ISLNK(info.st_mode) or not (stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)):
                raise ValueError('Unsupported output entry')
            if stat.S_ISREG(info.st_mode):
                total += info.st_size
    return total


def environment(output):
    return {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(output),
            'TMPDIR': str(output), 'LANG': 'en_US.UTF-8'}


def stop_child(process):
    # Only the fresh process group created below, never the profiled app.
    try:
        os.killpg(process.pid, signal.SIGINT)
        process.wait(timeout=3)
    except ProcessLookupError:
        pass
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=3)


def supervised(argv, output, seconds, budget=MAX_BYTES, minimum_free=MIN_FREE):
    private_dir(output)
    if not 0 < seconds <= 135 or not 0 < budget <= MAX_BYTES:
        raise ValueError('Invalid supervision budget')
    log = output / 'tool.log'
    fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    def limits():
        os.umask(0o077)
        resource.setrlimit(resource.RLIMIT_FSIZE, (budget, budget))
    process = None
    started = time.monotonic()
    try:
        with os.fdopen(fd, 'wb') as stream:
            process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=stream,
                stderr=stream, cwd=output, env=environment(output), start_new_session=True,
                preexec_fn=limits)
            while True:
                if size_under(output) >= budget:
                    raise RuntimeError('Output budget exceeded; partial evidence preserved')
                if shutil.disk_usage(output).free < minimum_free:
                    raise RuntimeError('Less than 2 GiB free; partial evidence preserved')
                status = process.poll()
                if status is not None:
                    if status:
                        raise RuntimeError('Profiler failed; inspect private log locally')
                    return
                if time.monotonic() - started >= seconds:
                    raise RuntimeError('Profiler deadline reached; partial evidence preserved')
                time.sleep(0.1)
    finally:
        if process is not None and process.poll() is None:
            stop_child(process)


def tool():
    result = subprocess.run(['/usr/bin/xcrun', '--find', 'xctrace'],
        env={'PATH': '/usr/bin:/bin:/usr/sbin:/sbin'}, stdin=subprocess.DEVNULL,
        capture_output=True, timeout=5)
    if result.returncode:
        raise ValueError('xctrace unavailable; no permissions or tools changed')
    path = no_links(Path(result.stdout.decode().strip()))
    if not path.is_absolute() or not path.is_file():
        raise ValueError('Invalid xctrace path')
    for entry in [path, *path.parents]:
        if entry.stat().st_uid != 0 or entry.stat().st_mode & 0o022:
            raise ValueError('xctrace must reside under root-owned non-writable paths')
        if entry == Path('/Applications'):
            break
    return path


def candidate(receipt):
    receipt = no_links(Path(receipt))
    if not receipt.is_relative_to(ROOT / 'Resources/Contracts') or receipt.stat().st_size > 65536:
        raise ValueError('Use a project package receipt')
    r = json.loads(receipt.read_text())
    if not r.get('validation_instance') or r.get('bundle_id') != 'dev.local.TerminalWorkspaceRust.Development':
        raise ValueError('Only the synthetic Development candidate is allowed')
    bundle = no_links(Path(r['bundle']))
    runtime = no_links(Path((ROOT / '.runtime-location').read_text().strip()))
    if not bundle.is_relative_to(runtime / 'staged'):
        raise ValueError('Candidate must be in the managed stage')
    resources = bundle / 'Contents/Resources'
    if not (resources / 'validation-profile').is_file() or (resources / 'validation-instance').read_text().strip() != r['validation_instance']:
        raise ValueError('Synthetic profile identity mismatch')
    info = plistlib.loads((bundle / 'Contents/Info.plist').read_bytes())
    if info.get('CFBundleIdentifier') != r['bundle_id']:
        raise ValueError('Bundle identity mismatch')
    binary = no_links(bundle / 'Contents/MacOS/terminal-workspace')
    digest = hashlib.sha256()
    with binary.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    if digest.hexdigest() != r['signed_binary_sha256']:
        raise ValueError('Candidate binary hash mismatch')
    subprocess.run(['/usr/bin/codesign', '--verify', '--strict', str(bundle)],
        check=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, timeout=10)
    return binary, r


def process_matches(pid, binary):
    if pid <= 1:
        raise ValueError('Invalid PID')
    result = subprocess.run(['/usr/sbin/lsof', '-a', '-p', str(pid), '-d', 'txt', '-Fpun'],
        capture_output=True, text=True, timeout=5, check=True)
    fields = result.stdout.splitlines()
    if 'p' + str(pid) not in fields or 'u' + str(os.getuid()) not in fields:
        raise ValueError('PID must belong to this user')
    if 'n' + str(binary) not in fields:
        raise ValueError('PID does not match the exact candidate executable')


def output_dir():
    base = Path('/private/tmp') / ('terminal-workspace-profiles-' + str(os.getuid()))
    base.mkdir(mode=0o700, exist_ok=True)
    private_dir(base)
    return Path(tempfile.mkdtemp(prefix='capture-', dir=base))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['preflight', 'record', 'export'])
    parser.add_argument('--receipt', required=True)
    parser.add_argument('--pid', type=int)
    parser.add_argument('--seconds', type=int, default=30, choices=range(5, 121), metavar='5..120')
    parser.add_argument('--capture', type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        binary, receipt = candidate(args.receipt)
        executable = tool()
        if args.command == 'preflight':
            print(json.dumps({'available': True, 'validation_instance': receipt['validation_instance']}))
            return 0
        if args.command == 'record':
            if args.pid is None or args.capture is not None:
                raise ValueError('record requires exact PID and no existing capture')
            process_matches(args.pid, binary)
            out = output_dir()
            argv = [str(executable), 'record', '--template', 'Time Profiler', '--time-limit',
                str(args.seconds) + 's', '--output', str(out / 'capture.trace'), '--attach', str(args.pid)]
            supervised(argv, out, args.seconds + 10)
            process_matches(args.pid, binary)
        else:
            if args.capture is None or args.pid is not None:
                raise ValueError('export requires a managed capture, no PID')
            capture = private_dir(args.capture)
            base = Path('/private/tmp') / ('terminal-workspace-profiles-' + str(os.getuid()))
            if capture.parent != private_dir(base):
                raise ValueError('Only managed captures may be exported')
            previous = json.loads((capture / 'receipt.json').read_text())
            if previous['binary_sha256'] != receipt['signed_binary_sha256']:
                raise ValueError('Capture belongs to a different candidate')
            trace = no_links(capture / 'capture.trace')
            if size_under(capture) > MAX_BYTES:
                raise ValueError('Input capture exceeds budget')
            out = output_dir()
            argv = [str(executable), 'export', '--input', str(trace), '--xpath',
                '/trace-toc/run[@number="1"]/data/table[@schema="time-sample"]',
                '--output', str(out / 'samples.xml')]
            supervised(argv, out, 60)
        exclusive(out / 'receipt.json', json.dumps({'command': args.command,
            'binary_sha256': receipt['signed_binary_sha256'], 'validation_instance': receipt['validation_instance'],
            'max_bytes': MAX_BYTES, 'bytes': size_under(out), 'raw_data_private': True}, indent=2) + '\n')
        # Never print raw trace, logs, environment, symbols or process inventory.
        print(json.dumps({'result_directory': str(out), 'raw_data_private': True}))
        return 0
    except (ValueError, RuntimeError, OSError, subprocess.SubprocessError, KeyError, json.JSONDecodeError):
        print('Profiling stopped or preflight unavailable; no permissions changed. Preserve private evidence.')
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
