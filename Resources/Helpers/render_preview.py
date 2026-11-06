#!/usr/bin/env python3
"""Render an explicitly selected local document in an isolated, network-denied child."""
import json
import importlib.util
import os
from pathlib import Path
import stat
import subprocess
import signal
import threading
import sys


def tex_assets():
    spec = importlib.util.spec_from_file_location('workspace_tex_assets', Path(__file__).with_name('tex_assets.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sandbox_profile(directory, source_root, native=(), cache=None, network=False, browser=False):
    # macOS Seatbelt constrains local document reads as well as network access.
    # Chrome gets an empty private profile. TeX gets untrusted mode and cached bundles only.
    allowed = ['/System', '/usr/lib', '/usr/share', '/usr/bin', '/bin', '/sbin', '/Library/Fonts',
               '/private/var/db/dyld', '/dev', str(source_root), str(directory)]
    if cache:
        allowed.append(str(cache))
    if not native:
        allowed = ['/System', '/usr', '/bin', '/sbin', '/Library', '/Applications/Google Chrome.app', '/private/var/db', '/dev', str(source_root), str(directory)]
    # macOS aborts even /bin/echo during sandbox startup without reading the
    # root directory itself. A literal permits that directory, not its subtree.
    profile = '(version 1)(allow default)' + ('' if network else '(deny network*)') + '(deny file-read*)(deny file-write*)(allow file-read-metadata)(allow file-read* (literal "/"))' + '(allow file-write* (subpath ' + json.dumps(str(directory)) + '))' + ''.join(
        '(allow file-read* (subpath ' + json.dumps(p) + '))' for p in allowed)
    if browser:
        profile += '(allow file-write* (literal "/dev/null"))'
        # Chrome's private ProcessSingleton uses AF_UNIX beneath its temp dir.
        # Path filters never match TCP/IP or sockets outside this render output.
        for operation, side in [('network-bind', 'local'), ('network-inbound', 'local'), ('network-outbound', 'remote')]:
            profile += '(allow ' + operation + ' (' + side + ' unix-socket (subpath ' + json.dumps(str(directory)) + ')))'
    return profile + ''.join('(allow file-read* (literal ' + json.dumps(str(p)) + '))' for p in native)


def run(args, directory, source_root, native=(), cache=None, browser=False, redact_errors=False):
    profile = sandbox_profile(directory, source_root, native, cache, browser=browser)
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(directory), 'TMPDIR': str(directory), 'LANG': 'en_US.UTF-8'}
    if browser:
        # Chromium's CrashReporterClient::Create honors this independently of
        # --user-data-dir on macOS. Never permit the personal Crashpad path.
        env['BREAKPAD_DUMP_LOCATION'] = str(directory / 'crashpad')
        env['MAC_CHROMIUM_TMPDIR'] = str(directory)
    if cache:
        env['TECTONIC_CACHE_DIR'] = str(cache)
    child = subprocess.Popen(['/usr/bin/sandbox-exec', '-p', profile, *args], cwd=directory,
                             env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                             stderr=subprocess.PIPE, start_new_session=True)
    errors = bytearray()
    def drain():
        while True:
            chunk = child.stderr.read(4096)
            if not chunk:
                break
            errors.extend(chunk[:max(0, 8192-len(errors))])
    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    try:
        code = child.wait(timeout=25)
    except subprocess.TimeoutExpired:
        raise RuntimeError('Render cancelado después de 25 segundos')
    finally:
        # A browser can exit before its helpers. Reap the owned group even when
        # the leader has already returned, without inspecting other processes.
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()
        reader.join(timeout=1)
    if code:
        if redact_errors:
            raise RuntimeError('Preview HTML: el renderer aislado falló, código ' + str(code))
        detail = errors.decode('utf-8', errors='replace')[-1600:]
        raise RuntimeError('Render aislado falló, código ' + str(code) + ':\n' + detail)


def copy_html(source, root, output):
    source, root, output = Path(source), Path(root), Path(output)
    if source.resolve(strict=True) != source or not source.is_relative_to(root.resolve(strict=True)):
        raise RuntimeError('HTML: archivo fuera del proyecto o symlink')
    info = output.lstat()
    if output.resolve(strict=True) != output or not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('HTML: salida debe ser un directorio privado')
    fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > 2 * 1024**2:
            raise RuntimeError('HTML: documento supera2MiB o no es regular')
        content = stream.read(2 * 1024**2 + 1)
        after = os.fstat(stream.fileno())
        if len(content) > 2 * 1024**2 or len(content) != before.st_size or before.st_size != after.st_size or before.st_mtime_ns != after.st_mtime_ns:
            raise RuntimeError('HTML: documento cambió durante la copia')
    try:
        content.decode('utf-8')
    except UnicodeDecodeError:
        raise RuntimeError('HTML: documento debe usar UTF-8') from None
    copied = output / 'document.html'
    fd = os.open(copied, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(content)
    return copied


def render_html(source, root, output, binary, redact_errors=True):
    binary = Path(binary)
    if binary.resolve(strict=True) != binary or not binary.is_file() or not os.access(binary, os.X_OK):
        raise RuntimeError('HTML: renderer nativo no disponible; volvé a empaquetar la app')
    copied = copy_html(source, root, output)
    # Only the private document copy, bundled code and system resources enter
    # the renderer. It does not receive the project's filesystem read access.
    try:
        run([str(binary), str(copied), str(output / 'preview.png')], output,
            binary.parent.parent, redact_errors=redact_errors)
    finally:
        copied.unlink(missing_ok=True)


def main():
    kind, source, root, output = sys.argv[1:]
    source, root, output = Path(source), Path(root), Path(output)
    if source.resolve() != source or not source.is_relative_to(root.resolve()):
        raise ValueError('Source outside explicit project')
    info = output.lstat()
    if output.resolve(strict=True) != output or not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError('Output is not an owned private preview job')
    fd = os.open(output / '.owned-preview', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.getuid() or info.st_mode & 0o077 or stream.read(64) != b'terminal-workspace-preview-v1\n':
            raise ValueError('Invalid preview ownership marker')
    png = output / 'preview.png'
    if kind == 'html':
        binary = Path(__file__).resolve().parents[2] / 'MacOS/html-preview'
        render_html(source, root, output, binary)
    elif kind == 'tex':
        assets = tex_assets()
        cache, receipt = assets.load(output.parent)
        native = [entry['path'] for entry in receipt['runtime_files']]
        run([receipt['tectonic'], '--untrusted', '--only-cached', '--bundle', assets.BUNDLE, '--outdir', str(output), str(source)], output, root, native, cache)
        pdf = output / (source.stem + '.pdf')
        run([receipt['converter'], '-f', '1', '-singlefile', '-scale-to', '2200', '-png', str(pdf), str(output / 'preview')], output, output, native)
    else:
        raise ValueError('Unknown preview kind')
    if not png.is_file() or png.stat().st_size > 16 * 1024 * 1024:
        raise RuntimeError('El renderer no produjo un PNG dentro del límite')
    print(str(png))


if __name__ == '__main__':
    os.umask(0o077)
    try:
        def stopped(_signal, _frame):
            raise TimeoutError('Render interrumpido o fuera de plazo')
        signal.signal(signal.SIGALRM, stopped)
        signal.signal(signal.SIGTERM, stopped)
        signal.alarm(90)
        main()
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
