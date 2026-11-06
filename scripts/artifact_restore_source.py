#!/usr/bin/env python3
"""Reconstruct a source-history manifest into a new private temporary path.

The existing tree and content store are never modified. Failure leaves the
new destination for inspection. This tool has not yet been validated end to end.
"""
import argparse
import base64
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import zlib

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('artifact_retention', ROOT / 'scripts/artifact_retention.py')
retention = importlib.util.module_from_spec(spec)
spec.loader.exec_module(retention)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise RuntimeError('Duplicate manifest key')
        result[key] = value
    return result


def restore(manifest, destination):
    history = retention.common_git_dir(ROOT) / 'twr-source-history'
    if history.is_symlink() or manifest.parent != history / 'manifests' or manifest.parent.is_symlink() or manifest.is_symlink():
        raise RuntimeError('Manifest must be in the private project source store')
    if not re.fullmatch(r'[0-9a-f]{64}\.json', manifest.name):
        raise RuntimeError('Invalid manifest name')
    if not manifest.is_file() or manifest.stat().st_size > 16 * 1024**2:
        raise RuntimeError('Invalid manifest')
    raw = manifest.read_bytes()
    if hashlib.sha256(raw).hexdigest() != manifest.stem:
        raise RuntimeError('Manifest digest mismatch')
    data = json.loads(raw, object_pairs_hook=unique_object)
    entries = data.get('entries')
    if data.get('schema') != 1 or not isinstance(entries, dict) or not 1 <= len(entries) <= 20000:
        raise RuntimeError('Unsupported source manifest')
    parent = destination.parent
    if (not destination.is_absolute() or destination.exists() or destination.is_symlink()
            or parent.parent != Path('/private/tmp') or parent.is_symlink() or not parent.is_dir()
            or not re.fullmatch(r'twr-(?:source-recovery|rollback-source)-[0-9a-f]{32}', parent.name)):
        raise RuntimeError('Destination must be new inside a private registered recovery directory')
    info = parent.stat()
    if info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('Recovery directory must be private and owned')
    if entries.get('', {}).get('kind') != 'dir':
        raise RuntimeError('Manifest root is not a directory')
    expected = {}
    total = 0
    for name, item in entries.items():
        if not isinstance(name, str) or not isinstance(item, dict):
            raise RuntimeError('Invalid source entry')
        if name and (name.startswith('/') or '\\' in name or any(part in ('', '.', '..') for part in name.split('/'))):
            raise RuntimeError('Unsafe source path')
        kind = item.get('kind')
        if kind not in ('dir', 'file') or (name and name.rpartition('/')[0] not in entries):
            raise RuntimeError('Invalid source tree')
        mode, uid, gid, flags = (item.get(k) for k in ('mode', 'uid', 'gid', 'flags'))
        if (not all(isinstance(v, int) and v >= 0 for v in (mode, uid, gid, flags))
                or mode > 0o7777 or uid != os.getuid()):
            raise RuntimeError('Unsupported ownership or mode')
        attrs = item.get('xattrs')
        if not isinstance(attrs, list) or len(attrs) > 1024:
            raise RuntimeError('Invalid source attributes')
        decoded_attrs = []
        for pair in attrs:
            if not isinstance(pair, list) or len(pair) != 2:
                raise RuntimeError('Invalid source attribute')
            decoded_attrs.append(tuple(base64.b64decode(part, validate=True) for part in pair))
        acl = item.get('acl')
        acl = base64.b64decode(acl, validate=True) if acl is not None else None
        metadata = (decoded_attrs, acl)
        if kind == 'dir':
            expected[name] = (kind, mode, uid, gid, flags, metadata)
        else:
            digest = item.get('object')
            size = item.get('size')
            if (not isinstance(digest, str) or not re.fullmatch(r'[0-9a-f]{64}', digest)
                    or not isinstance(size, int) or not 0 <= size <= 32 * 1024**2):
                raise RuntimeError('Invalid source object')
            expected[name] = (kind, mode, uid, gid, flags, metadata, bytes.fromhex(digest))
    # The private recovery directory may inherit a different group from the
    # source (for example /private/tmp is wheel while app resources are admin).
    # Set only groups this user belongs to, and only on freshly created paths.
    # If a group cannot be reproduced, keep the reconstruction and stop before
    # the caller can retire the original artifact.
    allowed_groups = set(os.getgroups()) | {os.getgid()}

    def restore_group(path, uid, gid):
        info = path.lstat()
        if info.st_uid != uid:
            raise RuntimeError('Recovery owner differs; destination retained')
        if info.st_gid != gid:
            if gid not in allowed_groups:
                raise RuntimeError('Recovery group is not available to this user; destination retained')
            os.chown(path, -1, gid, follow_symlinks=False)
            info = path.lstat()
        if (info.st_uid, info.st_gid) != (uid, gid):
            raise RuntimeError('Recovery owner/group differs; destination retained')

    destination.mkdir(mode=0o700)
    restore_group(destination, expected[''][2], expected[''][3])
    names = sorted((name for name in entries if name), key=lambda name: (name.count('/'), name))
    for name in names:
        target = destination / name
        item = entries[name]
        if item['kind'] == 'dir':
            target.mkdir(mode=0o700)
            restore_group(target, item['uid'], item['gid'])
            continue
        obj = history / 'objects' / item['object']
        if obj.parent.is_symlink() or obj.is_symlink() or not obj.is_file() or obj.stat().st_size > 40 * 1024**2:
            raise RuntimeError('Source object unavailable')
        decoder = zlib.decompressobj()
        content = decoder.decompress(obj.read_bytes(), 32 * 1024**2 + 1)
        total += len(content)
        if (len(content) != item['size'] or len(content) > 32 * 1024**2 or total > 128 * 1024**2 or not decoder.eof or decoder.unused_data
                or hashlib.sha256(content).hexdigest() != item['object']):
            raise RuntimeError('Source object failed integrity verification')
        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'wb') as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        restore_group(target, item['uid'], item['gid'])
    libc = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
    libc.acl_from_text.argtypes = [ctypes.c_char_p]
    libc.acl_from_text.restype = ctypes.c_void_p
    libc.acl_set_link_np.argtypes = [ctypes.c_char_p, ctypes.c_int, ctypes.c_void_p]
    libc.acl_set_link_np.restype = ctypes.c_int
    libc.acl_free.argtypes = [ctypes.c_void_p]
    libc.acl_free.restype = ctypes.c_int
    libc.setxattr.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p,
                              ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    libc.setxattr.restype = ctypes.c_int
    for name in sorted(entries, key=lambda value: (value.count('/'), value), reverse=True):
        target = destination / name
        kind, mode, uid, gid, flags, metadata = expected[name][:6]
        if (target.stat().st_uid, target.stat().st_gid) != (uid, gid):
            raise RuntimeError('Recovery owner/group differs; destination retained')
        if stat.S_IMODE(target.stat().st_mode) != mode:
            os.chmod(target, mode, follow_symlinks=False)
        attrs, acl = metadata
        current_attrs, current_acl = retention.extended_metadata(target)
        current_attrs = dict(current_attrs)
        for key, value in attrs:
            if current_attrs.get(key) == value:
                continue
            buf = ctypes.create_string_buffer(value, len(value) or 1)
            if libc.setxattr(os.fsencode(target), key, buf, len(value), 0, 1) != 0:
                raise OSError(ctypes.get_errno(), 'Cannot restore extended attribute')
        if acl is not None and acl != current_acl:
            parsed = libc.acl_from_text(acl)
            if not parsed:
                raise RuntimeError('Cannot parse saved ACL')
            try:
                if libc.acl_set_link_np(os.fsencode(target), 0x100, parsed) != 0:
                    raise OSError(ctypes.get_errno(), 'Cannot restore ACL')
            finally:
                libc.acl_free(parsed)
        if flags != getattr(target.stat(), 'st_flags', 0):
            os.chflags(target, flags, follow_symlinks=False)
    if retention.source_files(destination) != expected:
        raise RuntimeError('Restored metadata or contents differ; destination retained')
    return {'destination': str(destination), 'manifest_sha256': manifest.stem,
            'native_source_sha256': data.get('native_source_sha256')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--destination', type=Path, required=True)
    args = parser.parse_args()
    with retention.project_lock(ROOT):
        print(json.dumps(restore(args.manifest, args.destination), indent=2))


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError, zlib.error) as error:
        raise SystemExit('ERROR: ' + str(error))
