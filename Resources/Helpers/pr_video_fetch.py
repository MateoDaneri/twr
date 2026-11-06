#!/usr/bin/env python3
"""Explicit GitHub PR media requests. No imports execute requests or read auth."""
import http.client
import ipaddress
import json
import os
from pathlib import Path
import re
import select
import shutil
import socket
import ssl
import stat
import subprocess
import sys
import time
from urllib.parse import unquote, urljoin, urlsplit

MAX_INPUT = 100 * 1024**2
MAX_METADATA = 1024**2
CDN = 'github-production-user-asset-6210df.s3.amazonaws.com'
HOSTS = {'github.com', 'user-images.githubusercontent.com',
         'private-user-images.githubusercontent.com', CDN}
PARENT = os.getppid()


def alive():
    if os.getppid() != PARENT:
        raise RuntimeError('Video: la app se cerró')


def target(url, initial=False):
    if (not isinstance(url, str) or len(url) > 16384 or not url.isascii()
            or any(ord(c) <= 32 or ord(c) == 127 for c in url) or '\\' in url):
        raise RuntimeError('Video: URL de adjunto inválida')
    parsed = urlsplit(url)
    if (parsed.scheme != 'https' or parsed.hostname not in HOSTS
            or parsed.netloc.lower() != parsed.hostname or parsed.fragment):
        raise RuntimeError('Video: destino de adjunto no permitido')
    path = unquote(parsed.path)
    if ('..' in path or '\\' in path or any(ord(c) <= 32 or ord(c) == 127 for c in unquote(url))
            or re.search(r'%(?![0-9A-Fa-f]{2})', url)):
        raise RuntimeError('Video: ruta de adjunto inválida')
    host = parsed.hostname
    if host == 'github.com' and not re.fullmatch(r'/user-attachments/(?:assets/[A-Za-z0-9-]+|files/[A-Za-z0-9_./-]+)', path):
        raise RuntimeError('Video: ruta de adjunto no permitida')
    if initial and host == CDN:
        raise RuntimeError('Video: seleccioná un adjunto del PR')
    return parsed


def public_addresses(host, resolve=socket.getaddrinfo):
    addresses = resolve(host, 443, type=socket.SOCK_STREAM)
    if not addresses or len(addresses) > 64:
        raise RuntimeError('Video: destino sin dirección pública')
    result = []
    for family, _, _, _, address in addresses:
        ip = ipaddress.ip_address(address[0])
        if (not ip.is_global or ip.is_multicast
                or isinstance(ip, ipaddress.IPv6Address) and (ip.scope_id or ip.ipv4_mapped or ip.sixtofour or ip.teredo)):
            raise RuntimeError('Video: destino de red no público')
        if family not in (socket.AF_INET, socket.AF_INET6):
            raise RuntimeError('Video: familia de red no permitida')
        if str(ip) not in result:
            result.append(str(ip))
    return result


class PinnedHTTPS(http.client.HTTPSConnection):
    def connect(self):
        # Resolve once, reject every nonpublic result and connect to that literal
        # IP. TLS still verifies the original hostname and sends its SNI.
        addresses = public_addresses(self.host)
        last = None
        started = time.monotonic()
        for address in addresses:
            remaining = self.timeout - (time.monotonic() - started)
            if remaining <= 0:
                break
            try:
                raw = socket.create_connection((address, 443), remaining)
                try:
                    self.sock = self._context.wrap_socket(raw, server_hostname=self.host)
                except BaseException:
                    raw.close()
                    raise
                return
            except OSError as error:
                last = error
        raise RuntimeError('Video: no se pudo conectar al adjunto') from last


def request(parsed, headers, timeout):
    connection = PinnedHTTPS(parsed.hostname, timeout=timeout, context=ssl.create_default_context())
    try:
        path = parsed.path + ('?' + parsed.query if parsed.query else '')
        connection.request('GET', path, headers=headers)
        return connection, connection.getresponse()
    except BaseException:
        connection.close()
        raise


def gh(command, limit, seconds=20, input_bytes=b''):
    binary = shutil.which('gh', path='/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin')
    if binary is None:
        raise RuntimeError('Video: GitHub CLI no está disponible')
    env = os.environ.copy()
    for name in ('GH_DEBUG', 'DEBUG', 'GH_BROWSER', 'BROWSER', 'GH_PAGER', 'PAGER',
                 'GH_HOST', 'GH_REPO', 'GIT_TRACE', 'GIT_TRACE_CURL'):
        env.pop(name, None)
    env.update({'GH_HOST': 'github.com', 'GH_PROMPT_DISABLED': '1', 'GH_PAGER': 'cat',
                'GH_NO_UPDATE_NOTIFIER': '1', 'GH_NO_EXTENSION_UPDATE_NOTIFIER': '1'})
    process = subprocess.Popen([binary, *command], env=env, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        process.stdin.write(input_bytes)
        process.stdin.close()
        started, output = time.monotonic(), bytearray()
        while True:
            alive()
            if time.monotonic() - started > seconds:
                raise RuntimeError('Video: GitHub CLI fuera de tiempo')
            ready, _, _ = select.select([process.stdout], [], [], .1)
            if ready:
                chunk = os.read(process.stdout.fileno(), min(65536, limit + 1 - len(output)))
                if not chunk:
                    break
                output.extend(chunk)
                if len(output) > limit:
                    raise RuntimeError('Video: respuesta de GitHub excedida')
        if process.wait(timeout=3):
            raise RuntimeError('Video: comprobá el acceso al PR con gh')
        return bytes(output)
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=3)
        process.stdout.close()
        if not process.stdin.closed:
            process.stdin.close()


def token():
    value = gh(['auth', 'token', '--hostname', 'github.com'], 16384, 15).decode('ascii').strip()
    if not value or any(ord(c) <= 32 or ord(c) == 127 for c in value):
        raise RuntimeError('Video: credencial de GitHub inválida')
    return value


def metadata(repository, number, invoke=gh):
    if (not re.fullmatch(r'[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+', repository)
            or repository.split('/')[1] in ('.', '..') or type(number) is not int
            or not 1 <= number <= 2147483647):
        raise RuntimeError('Video: PR inválido')
    owner, name = repository.split('/')
    query = '''query($owner:String!,$name:String!,$number:Int!,$cursor:String){
repository(owner:$owner,name:$name){pullRequest(number:$number){body
comments(first:100,after:$cursor){nodes{body}pageInfo{hasNextPage endCursor}}}}}'''
    bodies, total, cursor = [], 0, None
    for page in range(3):
        variables = {'owner': owner, 'name': name, 'number': number, 'cursor': cursor}
        raw = invoke(['api', 'graphql', '--hostname', 'github.com', '--method', 'POST', '--input', '-'],
                     2 * MAX_METADATA, input_bytes=json.dumps({'query': query, 'variables': variables}).encode())
        response = json.loads(raw)
        if response.get('errors'):
            raise RuntimeError('Video: no se pudo leer el PR')
        pr = response['data']['repository']['pullRequest']
        values = ([pr['body']] if page == 0 else []) + [c['body'] for c in pr['comments']['nodes']]
        for body in values:
            if not isinstance(body, str):
                raise RuntimeError('Video: texto de PR inválido')
            total += len(body.encode())
            if total > MAX_METADATA or len(bodies) == 256:
                raise RuntimeError('Video: máximo 256 textos y 1 MiB total')
            bodies.append(body)
        info = pr['comments']['pageInfo']
        if not info['hasNextPage']:
            return bodies
        following = info['endCursor']
        if not isinstance(following, str) or len(following) > 4096 or following == cursor:
            raise RuntimeError('Video: paginación inválida')
        cursor = following
    raise RuntimeError('Video: demasiados comentarios del PR')


def review(repository, number, invoke=gh):
    if (not re.fullmatch(r'[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+', repository)
            or repository.split('/')[1] in ('.', '..') or type(number) is not int
            or not 1 <= number <= 2147483647):
        raise RuntimeError('PR: identificador inválido')
    endpoint = f'repos/{repository}/pulls/{number}'
    def read(path):
        raw = invoke(['api', '--hostname', 'github.com', '--method', 'GET', path], 4 * 1024**2, seconds=20)
        return json.loads(raw)
    data = read(endpoint)
    count = data.get('changed_files')
    if type(count) is not int or not 0 <= count <= 3000:
        raise RuntimeError('PR: límite de3000archivos; abrí la vista web')
    def clean(value, limit=65536):
        if not isinstance(value, str):
            return ''
        if len(value.encode()) > limit:
            raise RuntimeError('PR: texto supera el límite de la vista')
        return ''.join(c for c in value if c in '\n\t' or ord(c) >= 32 and ord(c) != 127)
    lines = [f'Git · PR {repository}#{number}', clean(data.get('title'), 1000),
             f"Estado: {data.get('state')} · draft: {data.get('draft')} · merged: {data.get('merged')} · mergeable: {data.get('mergeable')}",
             f"Base: {clean(data.get('base', {}).get('ref'), 1000)}  {clean(data.get('base', {}).get('sha'), 80)}",
             f"Head: {clean(data.get('head', {}).get('ref'), 1000)}  {clean(data.get('head', {}).get('sha'), 80)}",
             '', clean(data.get('body')), '', f'Archivos: {count} · +{data.get("additions")} / -{data.get("deletions")}', '']
    received = 0
    for page in range(1, (count + 99) // 100 + 1):
        values = read(endpoint + f'/files?per_page=100&page={page}')
        if not isinstance(values, list) or not 1 <= len(values) <= 100:
            raise RuntimeError('PR: listado de archivos incompleto')
        for item in values:
            lines += [f"{clean(item.get('filename'), 4096)} · {item.get('status')} · +{item.get('additions')} / -{item.get('deletions')}",
                      clean(item.get('patch'), 1024**2) or '(GitHub no entregó patch de este archivo)', '']
        received += len(values)
        if sum(len(x.encode()) for x in lines) > 4 * 1024**2:
            raise RuntimeError('PR: vista supera4MiB; abrí la vista web')
    if received != count:
        raise RuntimeError('PR: cambió el listado durante la consulta; volvé a consultar')
    return '\n'.join(lines)


def download(url, directory, send=request, get_token=token):
    parsed = target(url, initial=True)
    started, credential, authenticated, crossed, redirects = time.monotonic(), None, False, False, 0
    output = directory / 'attachment.part'
    while True:
        alive()
        remaining = 60 - (time.monotonic() - started)
        if remaining <= 0:
            raise RuntimeError('Video: descarga fuera de tiempo')
        headers = {'Accept': 'application/octet-stream', 'Accept-Encoding': 'identity',
                   'User-Agent': 'TerminalWorkspace-video/1'}
        if credential and parsed.hostname == 'github.com' and not crossed:
            headers['Authorization'] = 'token ' + credential
        connection, response = send(parsed, headers, min(10, remaining))
        try:
            if response.status in (401, 403, 404) and parsed.hostname == 'github.com' and not authenticated and not crossed:
                credential, authenticated = get_token(), True
                continue
            if response.status in (301, 302, 303, 307, 308):
                if redirects == 5:
                    raise RuntimeError('Video: demasiadas redirecciones')
                following = target(urljoin(parsed.geturl(), response.getheader('Location', '')))
                if following.hostname != parsed.hostname:
                    credential, crossed = None, True
                parsed, redirects = following, redirects + 1
                continue
            if response.status != 200 or response.getheader('Content-Encoding', 'identity') != 'identity':
                raise RuntimeError('Video: no se pudo descargar el adjunto')
            length = response.getheader('Content-Length')
            if length and (not length.isdigit() or int(length) > MAX_INPUT):
                raise RuntimeError('Video: adjunto supera100MiB')
            kind = response.getheader('Content-Type', '').split(';')[0].strip().lower()
            suffix = {'video/mp4': '.mp4', 'video/quicktime': '.mov', 'video/webm': '.webm'}.get(kind)
            if suffix is None and kind in ('application/octet-stream', 'binary/octet-stream'):
                suffix = Path(parsed.path).suffix.lower()
            if suffix not in ('.mp4', '.mov', '.webm'):
                raise RuntimeError('Video: el adjunto no declara un video compatible')
            count = 0
            with output.open('xb') as stream:
                while True:
                    alive()
                    if time.monotonic() - started > 60:
                        raise RuntimeError('Video: descarga fuera de tiempo')
                    if shutil.disk_usage(directory).free < 64 * 1024**2:
                        raise RuntimeError('Video: menos de64MiB libres')
                    chunk = response.read(65536)
                    if not chunk:
                        break
                    count += len(chunk)
                    if count > MAX_INPUT:
                        raise RuntimeError('Video: adjunto supera100MiB')
                    stream.write(chunk)
            if not count or length and count != int(length):
                raise RuntimeError('Video: descarga incompleta')
            destination = directory / ('attachment' + suffix)
            output.rename(destination)
            return destination
        finally:
            response.close()
            connection.close()


def main():
    data = json.loads(sys.stdin.buffer.read(16385))
    if sys.argv[1:] == ['review']:
        print(json.dumps({'text': review(data['repository'], data['number'])}))
        return
    if sys.argv[1:] == ['metadata']:
        print(json.dumps({'bodies': metadata(data['repository'], data['number'])}))
        return
    if len(sys.argv) != 4 or sys.argv[1] != 'download':
        raise RuntimeError('Video: operación inválida')
    job, decoder = Path(sys.argv[2]), Path(sys.argv[3])
    info = job.lstat()
    if (job.resolve(strict=True) != job or not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.getuid() or info.st_mode & 0o077):
        raise RuntimeError('Video: directorio privado inválido')
    received = job / 'received'
    received.mkdir(mode=0o700)
    source = download(data['url'], received)
    import importlib.util
    spec = importlib.util.spec_from_file_location('local_video', Path(__file__).with_name('prepare_video.py'))
    local = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(local)
    sys.argv = [str(spec.origin), str(source), str(received), str(job), str(decoder)]
    local.main()


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except Exception as error:
        # No response body, signed URL, subprocess stderr or token in errors.
        print(str(error) if isinstance(error, RuntimeError) else 'Video: no se pudo consultar el adjunto', file=sys.stderr)
        sys.exit(1)
