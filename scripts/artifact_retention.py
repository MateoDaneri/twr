"""Conservative retention for project-owned, reproducible build artifacts.

Call only while holding rust_workspace.exclusive_build_lock. Unknown artifacts,
unique source trees, referenced paths and anything that may be active are kept.
"""
import base64
import ctypes
import errno
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
import time
import uuid
import zlib
from contextlib import contextmanager


def common_git_dir(root):
    value = subprocess.check_output(['git', '-C', str(root), 'rev-parse', '--git-common-dir'], text=True).strip()
    path = Path(value)
    path = (path if path.is_absolute() else root / path).resolve(strict=True)
    if path.name != '.git' or not path.is_dir():
        raise RuntimeError('unexpected shared Git directory')
    return path


@contextmanager
def project_lock(root):
    """Serialize budgets across worktrees before acquiring a runtime lock."""
    path = common_git_dir(root) / 'twr-artifact-retention.lock'
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise RuntimeError('project retention lock must be a private owned file')
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise RuntimeError('Another project build or package is active') from exc
        yield
    finally:
        os.close(fd)


def project_storage(root):
    """Discover only registered project worktrees and their private runtimes."""
    main = common_git_dir(root).parent
    output = subprocess.check_output(['git', '-C', str(root), 'worktree', 'list', '--porcelain', '-z'])
    worktrees = []
    runtimes = set()
    for field in output.split(b'\0'):
        if not field.startswith(b'worktree '):
            continue
        worktree = Path(os.fsdecode(field[len(b'worktree '):]))
        if worktree.is_symlink() or not worktree.is_dir():
            raise RuntimeError('registered worktree is inaccessible or a symlink')
        if worktree != main and not worktree.is_relative_to(main / '.worktrees'):
            raise RuntimeError('registered worktree lies outside the project')
        worktrees.append(worktree)
        marker = worktree / '.runtime-location'
        if marker.is_symlink():
            raise RuntimeError('runtime marker is a symlink')
        if marker.exists():
            info = marker.lstat()
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077 or info.st_size > 1024:
                raise RuntimeError('runtime marker is not private and owned')
            candidate = Path(marker.read_text().strip())
        else:
            candidate = Path('/private/tmp') / ('terminal-workspace-rust-' + str(os.getuid()) + '-' + hashlib.sha256(str(worktree).encode()).hexdigest()[:12])
        if candidate.parent != Path('/private/tmp') or not re.fullmatch(r'terminal-workspace-rust-' + str(os.getuid()) + r'-[0-9a-f]{12}', candidate.name):
            raise RuntimeError('invalid project runtime path')
        if candidate.is_symlink():
            raise RuntimeError('project runtime is a symlink')
        if candidate.exists():
            if not candidate.is_dir() or candidate.stat().st_uid != os.getuid():
                raise RuntimeError('project runtime is not an owned directory')
            runtimes.add(candidate)
    if root not in worktrees:
        raise RuntimeError('current worktree is not registered')
    return worktrees, runtimes


def extended_metadata(path):
    """Compare macOS xattr values and ACL text, including on directories."""
    libc = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
    libc.listxattr.argtypes = [ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
    libc.listxattr.restype = ctypes.c_ssize_t
    libc.getxattr.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    libc.getxattr.restype = ctypes.c_ssize_t
    libc.acl_get_link_np.argtypes = [ctypes.c_char_p, ctypes.c_int]
    libc.acl_get_link_np.restype = ctypes.c_void_p
    libc.acl_to_text.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ssize_t)]
    libc.acl_to_text.restype = ctypes.c_void_p
    libc.acl_free.argtypes = [ctypes.c_void_p]
    libc.acl_free.restype = ctypes.c_int
    name = os.fsencode(path)
    size = libc.listxattr(name, None, 0, 1)
    if size < 0:
        raise OSError(ctypes.get_errno(), 'listxattr failed', str(path))
    buf = ctypes.create_string_buffer(size or 1)
    if size and libc.listxattr(name, buf, size, 1) != size:
        raise OSError(ctypes.get_errno(), 'xattrs changed', str(path))
    attrs = []
    for attr in sorted(part for part in buf.raw[:size].split(b'\0') if part):
        length = libc.getxattr(name, attr, None, 0, 0, 1)
        if length < 0:
            raise OSError(ctypes.get_errno(), 'getxattr failed', str(path))
        value = ctypes.create_string_buffer(length or 1)
        if length and libc.getxattr(name, attr, value, length, 0, 1) != length:
            raise OSError(ctypes.get_errno(), 'xattr changed', str(path))
        attrs.append((attr, value.raw[:length]))
    ctypes.set_errno(0)
    acl = libc.acl_get_link_np(name, 0x100)
    if not acl:
        error = ctypes.get_errno()
        if error not in (0, errno.ENOENT, errno.ENOTSUP):
            raise OSError(error, 'acl_get_link_np failed', str(path))
        return attrs, None
    try:
        length = ctypes.c_ssize_t()
        text = libc.acl_to_text(acl, ctypes.byref(length))
        if not text:
            raise OSError(ctypes.get_errno(), 'acl_to_text failed', str(path))
        try:
            return attrs, ctypes.string_at(text, length.value)
        finally:
            libc.acl_free(text)
    finally:
        libc.acl_free(acl)


def allocated_bytes(paths):
    """Conservative allocated blocks; APFS clones may be counted more than once."""
    seen = set()
    total = 0
    def visit(path):
        nonlocal total
        info = path.lstat()
        identity = (info.st_dev, info.st_ino)
        if identity in seen:
            return
        seen.add(identity)
        total += info.st_blocks * 512
        if stat.S_ISDIR(info.st_mode):
            with os.scandir(path) as entries:
                for entry in entries:
                    visit(Path(entry.path))
    for path in paths:
        visit(path)
    return total


def bounded_logical_bytes(paths, *, max_file=64 * 1024**2, max_total=192 * 1024**2, max_entries=20000):
    """Bound package inputs by logical size, including sparse files."""
    total = 0
    count = 0
    def visit(path):
        nonlocal total, count
        info = path.lstat()
        count += 1
        if count > max_entries:
            raise RuntimeError('Package inputs exceed entry limit')
        if stat.S_ISDIR(info.st_mode):
            with os.scandir(path) as scan:
                for entry in scan:
                    visit(Path(entry.path))
        elif stat.S_ISREG(info.st_mode):
            if info.st_size > max_file:
                raise RuntimeError('Package input exceeds per-file size limit')
            total += info.st_size
            if total > max_total:
                raise RuntimeError('Package inputs exceed total size limit')
        else:
            raise RuntimeError('Package inputs contain a symlink or special file')
    for path in paths:
        visit(path)
    return total, count


def capacity(root, *, kind, count, budget, reserve, label, include):
    _, runtimes = project_storage(root)
    entries = []
    for runtime in runtimes:
        directory = runtime / kind
        if directory.is_symlink():
            raise RuntimeError(f'{label}: refusing symlinked artifact directory')
        if not directory.exists():
            continue
        if not directory.is_dir():
            raise RuntimeError(f'{label}: artifact location is not a directory')
        entries.extend(entry for entry in directory.iterdir() if include(entry.name))
    size = allocated_bytes(entries)
    if len(entries) >= count or size + reserve > budget:
        raise RuntimeError(f'{label}: {len(entries)} artifacts, {size} allocated bytes; '
            f'limit {count} artifacts / {budget} bytes with {reserve} reserved. '
            'Unclassified or unique data was kept; see docs/RUNBOOK.html#artifact-retention.')


def archive_capacity(root, *, count, budget, reserve):
    """Count ZIPs under every registered worktree, without following symlinks."""
    worktrees, _ = project_storage(root)
    archives = []
    for worktree in worktrees:
        directory = worktree / 'dist'
        if directory.is_symlink():
            raise RuntimeError('Package archives: refusing symlinked artifact directory')
        if not directory.exists():
            continue
        if not directory.is_dir():
            raise RuntimeError('Package archives: artifact location is not a directory')
        archives.extend(path for path in directory.rglob('*.zip') if path.is_file() and not path.is_symlink())
    size = allocated_bytes(archives)
    if len(archives) >= count or size + reserve > budget:
        raise RuntimeError(f'Package archives: {len(archives)} artifacts, {size} allocated bytes; '
            f'limit {count} artifacts / {budget} bytes with {reserve} reserved. '
            'Existing archives were kept; see docs/RUNBOOK.html#artifact-retention.')


def no_open_files(path):
    """A failed or inconclusive lsof check always protects the candidate."""
    result = subprocess.run(['/usr/sbin/lsof', '-n', '-Fpcn', '+D', str(path)],
                            capture_output=True, text=True, timeout=90)
    if result.returncode not in (0, 1) or result.stderr.strip():
        return False
    return not result.stdout.strip() and result.returncode == 1


def no_open_file(path):
    """A failed or inconclusive lsof check protects the single file."""
    result = subprocess.run(['/usr/sbin/lsof', '-n', '-Fpcn', '--', str(path)],
                            capture_output=True, text=True, timeout=90)
    if result.returncode not in (0, 1) or result.stderr.strip():
        return False
    return not result.stdout.strip() and result.returncode == 1


def referenced(root, candidate):
    """Protect project documentation and installation receipts, including short IDs."""
    needles = (str(candidate), candidate.name)
    docs = root / 'docs'
    paths = []
    if docs.is_dir():
        for pattern in ('*.md', '*.html', '*.json'):
            paths.extend(docs.rglob(pattern))
    dist = root / 'dist'
    if dist.is_dir():
        paths.extend(dist.glob('installed-*.json'))
    for path in paths:
        if path.is_symlink():
            return True
        content = path.read_text(errors='replace')
        if any(needle in content for needle in needles):
            return True
    return False


def explicitly_pinned(root, candidate):
    """Only an explicit current pin protects a path; historical mentions do not.

    Unknown legacy stages and rollback bundles remain protected separately.
    Malformed, non-private or symlinked pin data fails closed.
    """
    pins = common_git_dir(root) / 'twr-retention-pins.json'
    if pins.is_symlink():
        raise RuntimeError('Retention pins are a symlink')
    if not pins.exists():
        return False
    info = pins.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_mode & 0o077 or info.st_size > 1024 * 1024):
        raise RuntimeError('Retention pins must be a private owned file')
    data = json.loads(pins.read_text())
    if not isinstance(data, dict) or data.get('schema') != 1 or not isinstance(data.get('paths'), list):
        raise RuntimeError('Invalid retention pin schema')
    for value in data['paths']:
        if not isinstance(value, str):
            raise RuntimeError('Invalid retention pin path')
        pin = Path(value)
        if not pin.is_absolute() or '..' in pin.parts or pin.resolve(strict=False) != pin:
            raise RuntimeError('Retention pin must be an absolute path without symlinks')
        if candidate == pin or candidate in pin.parents or pin in candidate.parents:
            return True
    return False


def finish_managed_stage(root, bundle, *, expected_source, expected_binary):
    """Mark one reviewed stage complete, without deleting or unpinning others.

    Caller must hold the project and runtime locks. Active processes remain
    protected independently even after this marker is updated.
    """
    bundle = Path(bundle)
    stage = bundle.parent
    _, runtimes = project_storage(root)
    if (not bundle.is_absolute() or '..' in bundle.parts or bundle.resolve(strict=True) != bundle
            or stage.parent.name != 'staged' or stage.parent.parent not in runtimes
            or stage.is_symlink() or bundle.is_symlink() or not bundle.is_dir()):
        return False
    marker = stage / '.twr-retention.json'
    if marker.is_symlink() or not marker.is_file():
        return False
    info = marker.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_mode & 0o077 or info.st_size > 4096):
        raise RuntimeError('Stage marker is not private and owned')
    data = json.loads(marker.read_text())
    if (data.get('schema') != 2 or data.get('retention_managed') is not True
            or data.get('bundle') != bundle.name or data.get('source_sha256') != expected_source
            or data.get('binary_sha256') != expected_binary
            or file_digest(bundle / 'Contents/MacOS/terminal-workspace', 64 * 1024**2).hex() != expected_binary):
        raise RuntimeError('Reviewed stage no longer matches its marker')
    data['in_flight'] = False
    fd, name = tempfile.mkstemp(prefix='.twr-marker-', dir=stage)
    temporary = Path(name)
    try:
        with os.fdopen(fd, 'w') as output:
            json.dump(data, output)
            output.write('\n')
            output.flush()
            os.fsync(output.fileno())
        if marker.is_symlink() or (marker.lstat().st_dev, marker.lstat().st_ino) != (info.st_dev, info.st_ino):
            raise RuntimeError('Stage marker changed before completion')
        os.replace(temporary, marker)
    finally:
        if temporary.exists() and not temporary.is_symlink():
            temporary.unlink()
    return True


def finish_managed_archive(root, archive, *, expected_source, expected_archive):
    """Mark one explicitly released ZIP no longer in flight; do not delete it."""
    archive = Path(archive)
    worktrees, _ = project_storage(root)
    if (archive.is_symlink() or not archive.is_file() or not archive.is_absolute()
            or '..' in archive.parts or archive.resolve(strict=True) != archive
            or not any(archive.is_relative_to(worktree / 'dist') for worktree in worktrees)):
        return False
    marker = archive.with_name(archive.name + '.twr-archive.json')
    if marker.is_symlink() or not marker.is_file():
        return False
    info = marker.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_mode & 0o077 or info.st_size > 4096):
        raise RuntimeError('Archive marker is not private and owned')
    data = json.loads(marker.read_text())
    if (data.get('schema') != 1 or data.get('retention_managed') is not True
            or data.get('archive') != str(archive) or data.get('source_sha256') != expected_source
            or data.get('archive_sha256') != expected_archive or file_digest(archive, 256 * 1024**2).hex() != expected_archive):
        raise RuntimeError('Released archive no longer matches its marker')
    data['in_flight'] = False
    fd, name = tempfile.mkstemp(prefix='.twr-marker-', dir=archive.parent)
    temporary = Path(name)
    try:
        with os.fdopen(fd, 'w') as output:
            json.dump(data, output)
            output.write('\n')
            output.flush()
            os.fsync(output.fileno())
        if marker.is_symlink() or (marker.lstat().st_dev, marker.lstat().st_ino) != (info.st_dev, info.st_ino):
            raise RuntimeError('Archive marker changed before completion')
        os.replace(temporary, marker)
    finally:
        if temporary.exists() and not temporary.is_symlink():
            temporary.unlink()
    return True


def active_source(build, source):
    """Protect current receipts and stages, not mentions in audit history."""
    for receipt in [build / 'source-snapshots/latest.json', *build.glob('build-receipt*.json')]:
        if receipt.is_symlink():
            return True
        if receipt.is_file():
            data = json.loads(receipt.read_text())
            if str(source) in (data.get('path'), data.get('source_snapshot')):
                return True
    stages = build / 'staged'
    if stages.is_symlink():
        return True
    if stages.is_dir():
        for marker in stages.glob('*/.twr-retention.json'):
            if marker.is_symlink() or json.loads(marker.read_text()).get('source_snapshot') == str(source):
                return True
    return False


def register_recovery(history, prefix):
    """Register an exact private /private/tmp reconstruction before creating it."""
    if prefix not in ('twr-source-recovery-', 'twr-rollback-source-'):
        raise RuntimeError('Unrecognized recovery prefix')
    registry = history / 'recoveries'
    if registry.is_symlink() or not registry.is_dir():
        raise RuntimeError('Recovery registry is not a real directory')
    info = registry.stat()
    if info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('Recovery registry must be private and owned')
    token = uuid.uuid4().hex
    container = Path('/private/tmp') / (prefix + token)
    if container.exists() or container.is_symlink():
        raise RuntimeError('Recovery path collision')
    marker = registry / (token + '.json')
    fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as output:
        json.dump({'schema': 1, 'path': str(container)}, output)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())
    container.mkdir(mode=0o700)
    return container / 'tree', marker


def finish_recovery(destination, marker):
    """Remove only the exact successful reconstruction created by this process."""
    if marker.is_symlink() or not marker.is_file():
        raise RuntimeError('Recovery marker changed; reconstruction preserved')
    data = json.loads(marker.read_text())
    container = destination.parent
    if (data != {'schema': 1, 'path': str(container)} or container.is_symlink()
            or not container.is_dir() or container.stat().st_uid != os.getuid()
            or container.stat().st_mode & 0o077 or destination.name != 'tree'
            or destination.is_symlink() or not destination.is_dir()
            or set(path.name for path in container.iterdir()) != {'tree'}):
        raise RuntimeError('Recovery identity changed; reconstruction preserved')
    shutil.rmtree(container)
    marker.unlink()


def protected_source(build, source):
    for receipt in [build / 'source-snapshots/latest.json', *build.glob('build-receipt*.json')]:
        if receipt.is_symlink():
            return True
        if receipt.is_file():
            data = json.loads(receipt.read_text())
            if str(source) in (data.get('path'), data.get('source_snapshot')):
                return True
    stages = build / 'staged'
    if stages.is_symlink():
        return True
    if stages.is_dir():
        for marker in stages.glob('*/.twr-retention.json'):
            if marker.is_symlink() or json.loads(marker.read_text()).get('source_snapshot') == str(source):
                return True
    journal = build / 'retention-actions.jsonl'
    if journal.is_symlink():
        return True
    if journal.is_file():
        with journal.open() as entries:
            for line in entries:
                if json.loads(line).get('source_snapshot') == str(source):
                    return True
    return False


def file_digest(path, max_bytes=None):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode):
        raise RuntimeError('unexpected source file type')
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        before = os.fstat(fd)
        if (info.st_dev, info.st_ino) != (before.st_dev, before.st_ino):
            raise RuntimeError('source changed before read')
        if max_bytes is not None and before.st_size > max_bytes:
            raise RuntimeError('artifact exceeds bounded read limit')
        digest = hashlib.sha256()
        total = 0
        while True:
            data = os.read(fd, 1024 * 1024)
            if not data:
                break
            total += len(data)
            if max_bytes is not None and total > max_bytes:
                raise RuntimeError('artifact grew beyond bounded read limit')
            digest.update(data)
        after = os.fstat(fd)
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise RuntimeError('source changed during read')
        return digest.digest()
    finally:
        os.close(fd)


def source_files(root, *, max_file=64 * 1024**2, max_total=256 * 1024**2, max_entries=20000):
    """Only exact regular-file trees with matching names and modes qualify."""
    result = {}
    total = 0
    def visit(path, relative):
        nonlocal total
        if len(result) >= max_entries:
            raise RuntimeError('artifact tree exceeds entry limit')
        before = path.lstat()
        mode = stat.S_IMODE(before.st_mode)
        if stat.S_ISDIR(before.st_mode):
            result[relative] = ('dir', mode, before.st_uid, before.st_gid, getattr(before, 'st_flags', 0), extended_metadata(path))
            with os.scandir(path) as scan:
                children = sorted(entry.name for entry in scan)
            for name in children:
                visit(path / name, relative + '/' + name if relative else name)
        elif stat.S_ISREG(before.st_mode):
            total += before.st_size
            if before.st_size > max_file or total > max_total:
                raise RuntimeError('artifact tree exceeds size limit')
            result[relative] = ('file', mode, before.st_uid, before.st_gid,
                                getattr(before, 'st_flags', 0), extended_metadata(path), file_digest(path, max_file))
        else:
            raise RuntimeError('snapshot has unsupported file type')
        after = path.lstat()
        if (before.st_dev, before.st_ino, before.st_mtime_ns, before.st_ctime_ns) != (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_ctime_ns):
            raise RuntimeError('snapshot changed during comparison')
    visit(root, '')
    return result


def archive_source_snapshot(root, build, source, *, fingerprint, expected_sha,
                            history_budget=512 * 1024**2):
    """Preserve verified source in a deduplicated private content store.

    The original is never removed here. Callers hold project/runtime locks and
    must separately review any deletion. Unique objects are never evicted to
    fit the budget; a full store requires an explicit recovery decision.
    """
    cache = build / 'source-snapshots'
    source = Path(source)
    if (cache.is_symlink() or source.parent != cache or source.is_symlink()
            or not source.is_dir()
            or not re.fullmatch(r'[0-9a-f]{64}(?:-[A-Za-z0-9_-]+)?', source.name)):
        raise RuntimeError('Source snapshot is not an exact managed snapshot')
    manifest_file = source / 'source-manifest.json'
    if manifest_file.is_symlink() or not manifest_file.is_file():
        raise RuntimeError('Missing source snapshot manifest')
    hashes = json.loads(manifest_file.read_text())
    original = source_files(source)
    files = {name for name, value in original.items()
             if value[0] == 'file' and name != 'source-manifest.json'}
    if (not isinstance(hashes, dict) or set(hashes) != files
            or any(original[name][-1].hex() != value for name, value in hashes.items())
            or fingerprint(source)[0] != expected_sha or not no_open_files(source)):
        raise RuntimeError('Source snapshot failed provenance or activity checks')
    return _store_verified_tree(root, source, original, expected_sha,
                                lambda: fingerprint(source)[0] == expected_sha and no_open_files(source),
                                history_budget, 'source')


def archive_stage_resources(root, stage, bundle, *, expected_sha, history_budget=512 * 1024**2):
    """Preserve resources (including non-source video data) before retirement."""
    resources = bundle / 'Contents/Resources'
    if stage.is_symlink() or bundle.is_symlink() or resources.is_symlink() or not resources.is_dir():
        raise RuntimeError('Invalid stage resources')
    original = source_files(resources)
    if not no_open_files(stage):
        raise RuntimeError('Stage may be active')
    return _store_verified_tree(root, resources, original, expected_sha,
                                lambda: no_open_files(stage), history_budget, 'stage_resources')


def _store_verified_tree(root, source, original, expected_sha, unchanged, history_budget, tree_kind):
    history = common_git_dir(root) / 'twr-source-history'
    if history.is_symlink():
        raise RuntimeError('Source history is a symlink')
    history.mkdir(mode=0o700, exist_ok=True)
    def private_dir(path):
        if path.is_symlink():
            raise RuntimeError('Source history directory is a symlink')
        path.mkdir(mode=0o700, exist_ok=True)
        info = path.stat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise RuntimeError('Source history must be private and owned')
    private_dir(history)
    objects = history / 'objects'
    manifests = history / 'manifests'
    recoveries = history / 'recoveries'
    private_dir(objects)
    private_dir(manifests)
    private_dir(recoveries)
    spec = importlib.util.spec_from_file_location('artifact_inventory', root / 'scripts/artifact_inventory.py')
    inventory = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(inventory)
    report = inventory.collect(root)
    usage = inventory.managed_usage(root, report, initialize=False)
    pool_remaining = usage['budget_bytes'] - usage['allocated_bytes'] - inventory.METADATA_RESERVE
    history_used = allocated_bytes([history]) + report['categories']['recovery_scratch']['allocated_bytes']

    def publish(destination, content):
        nonlocal pool_remaining, history_used
        if destination.is_symlink():
            raise RuntimeError('Source history entry is a symlink')
        if destination.exists():
            if not destination.is_file() or destination.read_bytes() != content:
                raise RuntimeError('Source history entry differs from its identity')
            return
        # Reserve both a temporary file and the final hardlink's metadata;
        # the inode itself is counted once. Existing unknown data is counted.
        reserve = ((len(content) + 4095) // 4096 + 4) * 4096
        if history_used + reserve > history_budget or reserve > pool_remaining:
            raise RuntimeError('Unique source history exceeds its budget; preserve and review')
        fd, name = tempfile.mkstemp(prefix='.pending-', dir=destination.parent)
        temporary = Path(name)
        try:
            with os.fdopen(fd, 'wb') as output:
                output.write(content)
                output.flush()
                os.fsync(output.fileno())
            if temporary.read_bytes() != content:
                raise RuntimeError('Source history writeback mismatch')
            os.link(temporary, destination, follow_symlinks=False)
            allocated = allocated_bytes([destination]) + 4 * 4096
            history_used += allocated
            pool_remaining -= allocated
        finally:
            if temporary.is_file() and not temporary.is_symlink():
                temporary.unlink()

    entries = {}
    for name, value in original.items():
        kind, mode, uid, gid, flags, metadata = value[:6]
        attrs, acl = metadata
        entry = {'kind': kind, 'mode': mode, 'uid': uid, 'gid': gid, 'flags': flags,
                 'xattrs': [[base64.b64encode(key).decode(), base64.b64encode(val).decode()]
                            for key, val in attrs],
                 'acl': base64.b64encode(acl).decode() if acl is not None else None}
        if kind == 'file':
            path = source / name
            if path.lstat().st_nlink != 1:
                raise RuntimeError('Snapshot contains a hardlinked file')
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(fd, 'rb') as stream:
                data = stream.read(32 * 1024**2 + 1)
            digest = value[-1].hex()
            if len(data) > 32 * 1024**2 or hashlib.sha256(data).hexdigest() != digest:
                raise RuntimeError('Snapshot file changed while preserving it')
            obj = objects / digest
            if obj.is_symlink():
                raise RuntimeError('Source object is a symlink')
            if not obj.exists():
                publish(obj, zlib.compress(data, level=6))
            if not obj.is_file() or obj.stat().st_size > 40 * 1024**2:
                raise RuntimeError('Invalid source object')
            decoder = zlib.decompressobj()
            decoded = decoder.decompress(obj.read_bytes(), 32 * 1024**2 + 1)
            # Check completeness and bounds before accepting an existing object.
            if (len(decoded) > 32 * 1024**2 or not decoder.eof or decoder.unused_data
                    or hashlib.sha256(decoded).hexdigest() != digest):
                raise RuntimeError('Source object failed readback')
            entry['object'] = digest
            entry['size'] = len(data)
        entries[name] = entry
    if source_files(source) != original or not unchanged():
        raise RuntimeError('Source changed while preserving it')
    payload = json.dumps({'schema': 1, 'tree_kind': tree_kind,
                          'native_source_sha256': expected_sha, 'entries': entries},
                         ensure_ascii=True, sort_keys=True, separators=(',', ':')).encode()
    identity = hashlib.sha256(payload).hexdigest()
    destination = manifests / (identity + '.json')
    publish(destination, payload)
    if hashlib.sha256(destination.read_bytes()).hexdigest() != identity:
        raise RuntimeError('Source manifest failed readback')
    # A content-addressed store alone is not enough: reconstruct into a new
    # private location and verify bytes and metadata before any caller may
    # retire a source tree. Preserve a failed reconstruction for inspection.
    restore_spec = __import__('importlib.util', fromlist=['spec_from_file_location'])
    spec = restore_spec.spec_from_file_location('twr_restore_source', root / 'scripts/artifact_restore_source.py')
    module = restore_spec.module_from_spec(spec)
    spec.loader.exec_module(module)
    recovery_reserve = sum((source / name).lstat().st_size for name, value in original.items()
                           if value[0] == 'file') + len(original) * 8192 + 16 * 1024**2
    if recovery_reserve > pool_remaining or history_used + recovery_reserve > history_budget:
        raise RuntimeError('Insufficient history budget for recoverability verification')
    restored, recovery_marker = register_recovery(history, 'twr-source-recovery-')
    module.restore(destination, restored)
    if source_files(restored) != original:
        raise RuntimeError('Recovered source differs; recovered tree retained at ' + str(restored))
    finish_recovery(restored, recovery_marker)
    return {'path': str(destination), 'sha256': identity,
            'native_source_sha256': expected_sha, 'store': str(history)}


def journal_preflight(root, build, events):
    """Reserve durable space for intent and every possible completion record."""
    path = build / 'retention-actions.jsonl'
    if path.is_symlink():
        raise RuntimeError('retention log is a symlink')
    size = 0
    if path.exists():
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise RuntimeError('Retention journal must be a private owned file')
        size = info.st_size
    # Extra allowance covers timestamps and small JSON formatting changes.
    needed = sum(len(json.dumps({'time': 9999999999.0, **event}).encode()) + 65 for event in events)
    if size + needed > 5 * 1024**2:
        raise RuntimeError('Retention journal lacks room for intent and completion')
    spec = importlib.util.spec_from_file_location('artifact_inventory', root / 'scripts/artifact_inventory.py')
    inventory = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(inventory)
    usage = inventory.managed_usage(root, inventory.collect(root), initialize=False)
    allocation = ((needed + 4095) // 4096 + 4) * 4096
    if usage['allocated_bytes'] + allocation > usage['budget_bytes']:
        raise RuntimeError('Managed pool lacks room for intent and completion')


def record(build, event):
    path = build / 'retention-actions.jsonl'
    if path.is_symlink():
        raise RuntimeError('retention log is a symlink')
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    line = (json.dumps({'time': time.time(), **event}) + '\n').encode()
    info = os.fstat(fd)
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
            or info.st_mode & 0o077 or info.st_size + len(line) > 5 * 1024**2):
        os.close(fd)
        raise RuntimeError('Retention journal must be a bounded private owned file')
    with os.fdopen(fd, 'ab') as output:
        output.write(line)
        output.flush()
        os.fsync(output.fileno())


def restore_missing(original, recovered, expected):
    """Repair an interrupted removal without overwriting surviving files."""
    if not original.exists():
        subprocess.run(['/usr/bin/ditto', '--rsrc', '--extattr', '--acl', '--qtn',
                        str(recovered), str(original)], check=True, timeout=180)
    remaining = source_files(original)
    if any(name not in expected or expected[name] != value for name, value in remaining.items()):
        raise RuntimeError('partial removal changed remaining files; retained archive requires review')
    for name in sorted((name for name in expected if name not in remaining), key=lambda name: (name.count('/'), name)):
        target = original / name
        if os.path.lexists(target):
            continue
        if not target.parent.is_dir():
            raise RuntimeError('cannot restore missing parent')
        subprocess.run(['/usr/bin/ditto', '--rsrc', '--extattr', '--acl', '--qtn',
                        str(recovered / name), str(target)], check=True, timeout=180)
    if source_files(original) != expected:
        raise RuntimeError('restoration incomplete; retained archive requires review')


def archive_and_remove_stage(root, build, directory, provenance):
    """Keep a fully verified archive before attempting to retire a dev stage."""
    dest = root / 'dist/retained-stage-archives'
    if dest.is_symlink():
        raise RuntimeError('retained archive directory is a symlink')
    archive_capacity(root, count=900, budget=24 * 1024**3, reserve=1024**3)
    dest.mkdir(mode=0o700, parents=True, exist_ok=True)
    archive = dest / (directory.name + '.zip')
    if archive.exists() or archive.is_symlink():
        raise RuntimeError('retained archive already exists')
    if allocated_bytes([directory]) > 1024**3:
        raise RuntimeError('development stage exceeds the archive reserve')
    created = False
    removal_started = False
    try:
        created = True
        subprocess.run(['/usr/bin/ditto', '-c', '-k', '--rsrc', '--extattr', '--acl', '--qtn',
                        '--sequesterRsrc', '--keepParent', str(directory), str(archive)],
                       check=True, timeout=180, capture_output=True)
        with tempfile.TemporaryDirectory(prefix='twr-retention-restore-', dir='/private/tmp') as tmp:
            subprocess.run(['/usr/bin/ditto', '-x', '-k', '--rsrc', '--extattr', '--acl', '--qtn',
                            str(archive), tmp], check=True, timeout=180, capture_output=True)
            recovered = Path(tmp) / directory.name
            expected = source_files(recovered)
            if source_files(directory) != expected:
                raise RuntimeError('archive does not preserve stage contents and metadata')
            recovered_app = recovered / 'Terminal Workspace Development.app'
            subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(recovered_app)],
                           check=True, timeout=30, capture_output=True)
            if not no_open_files(directory):
                raise RuntimeError('stage became active')
            event = {**provenance, 'archive': str(archive), 'archive_sha256': file_digest(archive).hex()}
            record(build, {'action': 'intent', 'kind': 'development_stage', **event})
            removal_started = True
            try:
                shutil.rmtree(directory)
            except OSError as error:
                restore_missing(directory, recovered, expected)
                record(build, {'action': 'removal_failed_restored', 'kind': 'development_stage', **event})
                raise RuntimeError('stage removal failed; stage restored and archive retained') from error
            record(build, {'action': 'removed', 'kind': 'development_stage', **event})
        return archive
    except BaseException:
        # Only an archive generated here, with its original intact, is disposable.
        # Once removal starts, retain it even if restoration encounters an error.
        if created and not removal_started and directory.is_dir() and archive.is_file() and not archive.is_symlink():
            archive.unlink()
        raise


def prune_previous_snapshot(root, build, previous, current):
    """Prune the previous build tree only if an exact source copy is retained."""
    cache = build / 'source-snapshots'
    previous, current = Path(previous), Path(current)
    if cache.is_symlink() or previous.parent != cache or current.parent != cache or previous == current:
        return False
    if not re.fullmatch(r'[0-9a-f]{64}(?:-[A-Za-z0-9_-]+)?', previous.name):
        return False
    if previous.is_symlink() or current.is_symlink() or not previous.is_dir() or not current.is_dir():
        return False
    if previous.name[:64] != current.name[:64] or protected_source(build, previous) or referenced(root, previous):
        return False
    if not no_open_files(previous) or source_files(previous) != source_files(current):
        return False
    if not no_open_files(previous):
        return False
    size = allocated_bytes([previous])
    record(build, {'action': 'intent', 'kind': 'source_snapshot', 'path': str(previous), 'retained': str(current), 'allocated_bytes': size})
    shutil.rmtree(previous)
    record(build, {'action': 'removed', 'kind': 'source_snapshot', 'path': str(previous), 'retained': str(current), 'allocated_bytes': size})
    return True


def prune_development_stages(root, build, current, *, fingerprint, keep=5, budget=512 * 1024**2):
    """Prune only completed, self-identifying synthetic stages from this policy."""
    staged = build / 'staged'
    if staged.is_symlink() or not staged.is_dir():
        return []
    managed = []
    for marker in staged.glob('*/.twr-retention.json'):
        directory = marker.parent
        if marker.is_symlink() or directory.is_symlink():
            continue
        data = json.loads(marker.read_text())
        if data.get('schema') != 1 or data.get('kind') != 'development':
            continue
        managed.append((marker.stat().st_mtime_ns, directory, data))
    managed.sort(reverse=True)
    total = sum(allocated_bytes([directory]) for _, directory, _ in managed)
    removed = []
    for index, (_, directory, data) in reversed(list(enumerate(managed))):
        if index < keep and total <= budget:
            continue
        bundle = directory / 'Terminal Workspace Development.app'
        source = Path(data.get('source_snapshot', ''))
        if index == 0 or directory == current or source.parent != build / 'source-snapshots' or source.is_symlink() or not source.is_dir():
            continue
        if set(p.name for p in directory.iterdir()) != {bundle.name, '.twr-retention.json'}:
            continue
        if bundle.is_symlink() or not bundle.is_dir() or referenced(root, directory):
            continue
        manifest = json.loads((bundle / 'Contents/Resources/build-manifest.json').read_text())
        executable = bundle / 'Contents/MacOS/terminal-workspace'
        if (not manifest.get('development_identity') or not manifest.get('validation_profile')
                or manifest.get('source_sha256') != data.get('source_sha256')
                or file_digest(executable).hex() != data.get('binary_sha256')):
            continue
        snapshot_manifest = source / 'source-manifest.json'
        if snapshot_manifest.is_symlink() or not snapshot_manifest.is_file():
            continue
        hashes = json.loads(snapshot_manifest.read_text())
        files = source_files(source)
        real_files = {name for name, entry in files.items() if entry[0] == 'file' and name != 'source-manifest.json'}
        if not isinstance(hashes, dict) or set(hashes) != real_files:
            continue
        if any(files[name][-1].hex() != sha for name, sha in hashes.items()):
            continue
        if fingerprint(source)[0] != data.get('source_sha256'):
            continue
        signature = subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(bundle)], capture_output=True, timeout=30)
        if signature.returncode or not no_open_files(directory):
            continue
        size = allocated_bytes([directory])
        provenance = {'path': str(directory), 'source_snapshot': str(source),
                      'source_sha256': data['source_sha256'], 'binary_sha256': data['binary_sha256'],
                      'build_manifest': manifest, 'allocated_bytes': size}
        archive_and_remove_stage(root, build, directory, provenance)
        total -= size
        removed.append(str(directory))
    return removed
