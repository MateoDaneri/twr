#!/usr/bin/env python3
"""Prepare only a fixed public TeX fixture; document rendering stays offline."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time

BUNDLE = 'https://relay.fullyjustified.net/default_bundle_v33.tar'
SOURCE = '\\documentclass{article}\n\\begin{document}\n\\section{Inicio}\\label{sec:inicio}\nTexto de prueba con Unicode: ñ.\nVer \\ref{sec:inicio}. $x^2+1$.\n\\end{document}\n'
LIMIT = 150 * 1024 * 1024
MACHO = {b'\xcf\xfa\xed\xfe', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca'}


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def private(path):
    if not path.is_absolute() or path.resolve() != path:
        raise ValueError('TeX requiere una ruta privada absoluta sin symlinks')
    path.mkdir(mode=0o700, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise ValueError('El directorio de recursos debe ser propio y privado, modo 0700')


def tools():
    tectonic = next((Path(p).resolve() for p in ['/opt/homebrew/bin/tectonic', '/usr/local/bin/tectonic'] if Path(p).is_file()), None)
    converter = next((p.resolve() for p in [Path('/opt/homebrew/bin/pdftoppm'), Path('/usr/local/bin/pdftoppm'), Path.home() / '.cache/codex-runtimes/codex-primary-runtime/dependencies/native/poppler/poppler/bin/pdftoppm'] if p.is_file()), None)
    if not tectonic or not converter:
        raise RuntimeError('Falta Tectonic o pdftoppm nativo; no se instalaron herramientas globales')
    return tectonic, converter


def macho(path):
    with path.open('rb') as stream:
        return stream.read(4) in MACHO


def runtime_files(tectonic, converter):
    pending = [(tectonic, tectonic), (converter, converter)]
    found = set()
    deadline = time.monotonic() + 30
    while pending:
        path, executable = pending.pop()
        path = path.resolve()
        if path in found:
            continue
        if len(found) >= 128 or time.monotonic() > deadline:
            raise RuntimeError('Dependencias nativas fuera de límites')
        if path not in (tectonic, converter) and not any(path.is_relative_to(root) for root in [Path('/opt/homebrew/Cellar'), Path('/usr/local/Cellar'), converter.parent.parent / 'lib']):
            raise RuntimeError('Dependencia fuera de las rutas de herramientas verificadas: ' + str(path))
        if not macho(path):
            raise RuntimeError('Se requiere un ejecutable Mach-O nativo: ' + str(path))
        found.add(path)
        output = subprocess.run(['/usr/bin/otool', '-L', str(path)], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=3).stdout.decode('utf-8')
        for line in output.splitlines()[1:]:
            name = line.strip().split(' (', 1)[0]
            if name.startswith(('/System/', '/usr/lib/')):
                continue
            if name.startswith('@rpath/'):
                tail = name[len('@rpath/'):]
                choices = [path.parent / tail, path.parent.parent / 'lib' / tail, executable.parent.parent / 'lib' / tail]
                dependency = next((p for p in choices if p.is_file()), None)
                if dependency is None:
                    raise RuntimeError('No se pudo resolver la biblioteca ' + name)
            elif name.startswith('@loader_path/'):
                dependency = path.parent / name[len('@loader_path/'):]
            elif name.startswith('@executable_path/'):
                dependency = executable.parent / name[len('@executable_path/'):]
            elif name.startswith('/'):
                dependency = Path(name)
            else:
                raise RuntimeError('Ruta de biblioteca no admitida: ' + name)
            pending.append((dependency, executable))
    return sorted(found)


def cache_manifest(cache):
    result = []
    total = 0
    for path in sorted(cache.rglob('*')):
        if path.is_symlink():
            raise ValueError('Symlink inesperado en recursos de TeX')
        if path.is_file():
            total += path.stat().st_size
            if total > LIMIT or len(result) >= 1024:
                raise ValueError('Recursos de TeX superan 150 MiB o 1024 archivos')
            result.append({'path': str(path.relative_to(cache)), 'size': path.stat().st_size, 'sha256': digest(path)})
    return result


def pointer(root, name):
    fd, temporary = tempfile.mkstemp(prefix='.current-', dir=root)
    with os.fdopen(fd, 'w') as stream:
        json.dump({'directory': name}, stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, root / 'current.json')


def load(state, name=None):
    root = state / 'tex-assets'
    private(state)
    private(root)
    if name is None:
        current = root / 'current.json'
        if not current.is_file():
            raise RuntimeError('Prepará los recursos una vez con :TexPrepare; el render no descarga paquetes')
        if current.is_symlink() or current.stat().st_size > 1024:
            raise ValueError('Puntero de recursos inválido')
        name = json.loads(current.read_text())['directory']
    if not isinstance(name, str) or len(name) > 80 or Path(name).name != name or not name.startswith('assets-'):
        raise ValueError('Directorio de recursos inválido')
    directory = root / name
    if directory.resolve() != directory:
        raise ValueError('Recursos contienen symlinks')
    receipt = directory / 'receipt.json'
    if receipt.is_symlink() or receipt.stat().st_size > 256 * 1024:
        raise ValueError('Recibo de recursos inválido')
    data = json.loads(receipt.read_text())
    if data['recipe_sha256'] != hashlib.sha256(SOURCE.encode()).hexdigest() or data['bundle'] != BUNDLE:
        raise ValueError('Los recursos pertenecen a otra preparación')
    cache = directory / 'cache'
    files = data['cache_files']
    if not isinstance(files, list) or not files or len(files) > 1024 or not cache.is_dir():
        raise ValueError('Manifest de recursos fuera de límites')
    if cache.resolve() != cache or cache_manifest(cache) != files:
        raise ValueError('Recurso de TeX cambió; se conserva para diagnóstico')
    installed = tuple(str(p) for p in tools())
    if installed != (data['tectonic'], data['converter']):
        raise ValueError('Las herramientas cambiaron; ejecutá :TexPrepare otra vez')
    native = data['runtime_files']
    if not isinstance(native, list) or len(native) > 128:
        raise ValueError('Lista de bibliotecas fuera de límites')
    verified = [{'path': str(p), 'sha256': digest(p)} for p in runtime_files(*(Path(p) for p in installed))]
    if native != verified:
        raise ValueError('Una herramienta o biblioteca cambió; ejecutá :TexPrepare')
    return cache, data


def prepare(state):
    private(state)
    root = state / 'tex-assets'
    private(root)
    tectonic, converter = tools()
    native = runtime_files(tectonic, converter)
    runtime = [{'path': str(p), 'sha256': digest(p)} for p in native]
    identity = hashlib.sha256((json.dumps(runtime, sort_keys=True) + SOURCE).encode()).hexdigest()[:24]
    name = 'assets-' + identity
    destination = root / name
    if destination.exists():
        load(state, name)
        pointer(root, name)
        return destination
    stage = Path(tempfile.mkdtemp(prefix='.preparing-', dir=root))
    for child in ['cache', 'home', 'out', 'tmp']:
        (stage / child).mkdir(mode=0o700)
    source = stage / 'bootstrap.tex'
    source.write_text(SOURCE)
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(stage / 'home'), 'TMPDIR': str(stage / 'tmp'), 'TECTONIC_CACHE_DIR': str(stage / 'cache'), 'LANG': 'en_US.UTF-8'}
    spec = importlib.util.spec_from_file_location('workspace_render_profile', Path(__file__).with_name('render_preview.py'))
    renderer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(renderer)
    profile = renderer.sandbox_profile(stage, stage, native, network=True)
    args = ['/usr/bin/sandbox-exec', '-p', profile, str(tectonic), '--untrusted', '--bundle', BUNDLE, '--outdir', str(stage / 'out'), str(source)]
    started = time.monotonic()
    errors = bytearray()
    child = subprocess.Popen(args, cwd=stage, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
    def drain():
        while True:
            chunk = child.stdout.read(4096)
            if not chunk:
                break
            errors.extend(chunk[:max(0, 65536-len(errors))])
    reader = threading.Thread(target=drain, daemon=True)
    reader.start()
    failure = None
    try:
        while child.poll() is None:
            total = sum(p.stat().st_size for p in stage.rglob('*') if p.is_file())
            if total > LIMIT or time.monotonic() - started > 180:
                failure = 'Preparación detenida por límite de tres minutos o 150 MiB'
                os.killpg(child.pid, signal.SIGKILL)
                break
            time.sleep(.5)
        code = child.wait()
    finally:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()
        reader.join(timeout=1)
        (stage / 'bootstrap.log').write_bytes(errors)
    if failure or code or not (stage / 'out/bootstrap.pdf').is_file():
        detail = errors.decode(errors='replace')[-1600:] or 'Sin salida de diagnóstico del proceso hijo'
        raise RuntimeError((failure or 'Tectonic aislado terminó con código ' + str(code) + ': ' + detail) + '\nSe conservó ' + str(stage))
    cache = stage / 'cache'
    files = cache_manifest(cache)
    data = {'schema': 1, 'recipe_sha256': hashlib.sha256(SOURCE.encode()).hexdigest(), 'bundle': BUNDLE, 'tectonic': str(tectonic), 'converter': str(converter), 'runtime_files': runtime, 'cache_files': files, 'seconds': round(time.monotonic()-started, 2)}
    (stage / 'receipt.json').write_text(json.dumps(data, indent=2) + '\n')
    for path in sorted(cache.rglob('*'), reverse=True):
        path.chmod(0o500 if path.is_dir() else 0o400)
    cache.chmod(0o500)
    if destination.exists():
        raise RuntimeError('Otro proceso preparó el destino; se conservaron ambas etapas')
    stage.rename(destination)
    load(state, name)
    pointer(root, name)
    return destination


if __name__ == '__main__':
    try:
        def stopped(_signal, _frame):
            raise TimeoutError('Preparación de TeX interrumpida o fuera de plazo')
        signal.signal(signal.SIGALRM, stopped)
        signal.signal(signal.SIGTERM, stopped)
        signal.alarm(240)
        print(prepare(Path(sys.argv[1])))
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
