#!/usr/bin/env python3
"""Package and operate the isolated Rust app. No user configuration or session reads."""
import argparse
import hashlib
import importlib.util
import json
import os
import re
import resource
from pathlib import Path
import plistlib
import shutil
import zipfile
import subprocess
import sys
import uuid
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
RUNTIME = ROOT / 'runtime/rust-app'
spec = importlib.util.spec_from_file_location('rust_workspace', ROOT / 'scripts/rust_workspace.py')
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)
BUNDLE = driver.BUILD / "bundle/Terminal Workspace Rust.app"


def build_html_renderer(source, target):
    if not os.access('/usr/bin/clang', os.X_OK):
        raise RuntimeError('HTML renderer requires the existing Apple Command Line Tools')
    flags = ['-fobjc-arc', '-fblocks', '-fno-modules', '-O2', '-Wall', '-Wextra', '-Werror',
             '-arch', 'arm64', '-mmacosx-version-min=12.0', '-framework', 'Cocoa', '-framework', 'WebKit']
    build = Path(tempfile.mkdtemp(prefix='html-build-', dir=target.parent.parent.parent.parent))
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(build), 'TMPDIR': str(build), 'LANG': 'en_US.UTF-8'}
    def limit_compiler_output():
        resource.setrlimit(resource.RLIMIT_FSIZE, (32 * 1024**2, 32 * 1024**2))
    with (build / 'compile.log').open('xb') as log:
        result = subprocess.run(['/usr/bin/clang', *flags, str(source), '-o', str(target)],
                                cwd=build, env=env, stdout=log, stderr=subprocess.STDOUT,
                                timeout=30, preexec_fn=limit_compiler_output)
    if result.returncode:
        raise RuntimeError('HTML renderer did not compile; compiler log retained in ' + str(build))
    dependencies = subprocess.check_output(['/usr/bin/otool', '-L', str(target)], text=True)
    for line in dependencies.splitlines()[1:]:
        if not line.strip().split(' (')[0].startswith(('/usr/lib/', '/System/Library/')):
            raise RuntimeError('HTML renderer has an unexpected runtime dependency')
    built_sha = hashlib.sha256(target.read_bytes()).hexdigest()
    subprocess.run(['/usr/bin/codesign', '--force', '--sign', '-', str(target)], check=True)
    subprocess.run(['/usr/bin/codesign', '--verify', '--strict', str(target)], check=True)
    # Keep diagnostics inside the signed bundle and remove only this invocation's
    # known one-file scratch directory after verifying the copy.
    log = build / 'compile.log'
    saved_log = target.parent.parent / 'Resources/BuildLogs/html-preview.log'
    saved_log.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(log, saved_log)
    if hashlib.sha256(log.read_bytes()).digest() != hashlib.sha256(saved_log.read_bytes()).digest():
        raise RuntimeError('HTML compiler log copy differs; scratch retained')
    if set(path.name for path in build.iterdir()) == {'compile.log'} and not log.is_symlink():
        log.unlink()
        build.rmdir()
    return {'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(), 'flags': flags,
            'build_sha256': built_sha, 'signed_sha256': hashlib.sha256(target.read_bytes()).hexdigest(),
            'dependencies': dependencies, 'compiler_log': 'Contents/Resources/BuildLogs/html-preview.log'}


def package(validation=False, staged=False, release=False, development=False, video_stage=None, archive_requested=False):
    # Keep receipt, executable and source stable throughout packaging; the
    # same lock is held by the build driver while it writes those artifacts.
    driver.prepare_build()
    driver.run_managed_retention((256 + (256 if archive_requested else 0)) * 1024**2)
    with driver.retention.project_lock(ROOT):
        with driver.exclusive_build_lock(driver.BUILD):
            driver.managed_preflight('stage' if staged else 'fixed_bundle',
                                     (256 + (256 if archive_requested else 0)) * 1024**2,
                                     archive=archive_requested)
            result = package_locked(validation, staged, release, development, video_stage, archive_requested)
    driver.run_managed_retention()
    return result


def package_locked(validation=False, staged=False, release=False, development=False, video_stage=None, archive_requested=False):
    instance = uuid.uuid4().hex[:8] if validation and staged else None
    bundle = driver.BUILD / "validation/Terminal Workspace Validation.app" if validation else BUNDLE
    profile = 'release' if release else 'debug'
    binary = driver.BUILD / 'target' / profile / 'terminal-workspace'
    if not binary.is_file():
        raise RuntimeError('Build first: python3 scripts/rust_workspace.py gui-build')
    receipt_path = driver.BUILD / ('build-receipt-release.json' if release else 'build-receipt.json')
    if not receipt_path.is_file():
        raise RuntimeError('No build receipt. Run gui-build before packaging.')
    receipt = json.loads(receipt_path.read_text())
    source_sha, source_files = driver.native_fingerprint()
    binary_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
    if receipt.get('source_sha256') != source_sha or receipt.get('binary_sha256') != binary_sha:
        raise RuntimeError('Native source/binary differ from the last successful gui-build. Rebuild before packaging.')
    if staged:
        driver.retention.capacity(ROOT, kind='staged', count=700, budget=24 * 1024**3,
            reserve=1024**3, label='Staged bundles', include=lambda name: True)
        if archive_requested:
            driver.retention.archive_capacity(ROOT, count=900, budget=24 * 1024**3, reserve=1024**3)
        # Never overwrite an earlier candidate, which may still be running.
        bundle = driver.BUILD / 'staged' / (source_sha[:12] + '-' + uuid.uuid4().hex[:8]) / bundle.name
    if development:
        bundle = bundle.with_name('Terminal Workspace Development.app')
    elif instance:
        bundle = bundle.with_name('Terminal Workspace Validation ' + instance + '.app')
    if driver.BUILD.is_symlink() or bundle.is_symlink():
        raise RuntimeError('Refusing a symlink as the generated bundle root.')
    if not staged and bundle.exists():
        raise RuntimeError('Existing non-staged bundle is preserved; package with --staged')
    # The reserve covers the signed app and metadata. Unusually large inputs
    # are rejected before copying instead of overrunning the shared pool.
    inputs = [ROOT / 'Resources']
    if video_stage:
        stage_root = Path(video_stage).resolve(strict=True)
        inputs.extend((stage_root / 'source.tar.xz', stage_root / 'bin'))
    binary_bytes, binary_entries = driver.retention.bounded_logical_bytes([binary], max_entries=1)
    estimate, entries = driver.retention.bounded_logical_bytes(inputs, max_file=32 * 1024**2, max_entries=8191)
    estimate += binary_bytes
    entries += binary_entries
    # Leave 64 MiB for the bounded helper executable/compiler log and 16 MiB
    # for signatures and metadata in addition to the measured input files.
    if estimate + entries * 8192 + 80 * 1024**2 > 256 * 1024**2:
        raise RuntimeError('Package inputs exceed the managed stage reservation')
    if staged:
        bundle.parent.mkdir(mode=0o700, parents=True, exist_ok=False)
        marker = bundle.parent / '.twr-retention.json'
        marker_fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(marker_fd, 'w') as output:
            json.dump({'schema': 2, 'retention_managed': True, 'in_flight': True,
                       'state': 'building', 'bundle': bundle.name,
                       'kind': 'development' if development else ('validation' if validation else 'release'),
                       'source_sha256': source_sha, 'source_snapshot': receipt['source_snapshot']}, output)
            output.write('\n')
            output.flush()
            os.fsync(output.fileno())
        marker_info = marker.lstat()
        marker_digest = driver.retention.file_digest(marker).hex()
    contents = bundle / 'Contents'
    executable = contents / 'MacOS/terminal-workspace'
    executable.parent.mkdir(parents=True, exist_ok=True)
    resources = contents / 'Resources/Demo/projects'
    resources.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, executable)
    linked = subprocess.check_output(['/usr/bin/otool', '-L', str(executable)], text=True)
    if 'libghostty-vt.dylib' in linked:
        raise RuntimeError('Terminal core must be embedded statically; runtime dylib dependency rejected')
    shutil.copytree(ROOT / 'Resources/Demo/projects', resources, dirs_exist_ok=True)
    shutil.copytree(ROOT / 'Resources/Helpers', contents / 'Resources/Helpers', dirs_exist_ok=True, ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
    shutil.copytree(ROOT / 'Resources/Notices', contents / 'Resources/Notices', dirs_exist_ok=True)
    html_source = Path(receipt['source_snapshot']) / 'Resources/Helpers/html_preview.m'
    if hashlib.sha256(html_source.read_bytes()).digest() != hashlib.sha256((ROOT / 'Resources/Helpers/html_preview.m').read_bytes()).digest():
        raise RuntimeError('HTML renderer differs from the frozen build snapshot')
    html_manifest = build_html_renderer(html_source, contents / 'MacOS/html-preview')
    video_manifest = None
    if video_stage:
        video_spec = importlib.util.spec_from_file_location('video_decoder', ROOT / 'scripts/video_decoder.py')
        video_driver = importlib.util.module_from_spec(video_spec)
        video_spec.loader.exec_module(video_driver)
        decoder_stage, video_receipt = video_driver.checked(video_stage)
        video = contents / 'Resources/Video'
        (video / 'bin').mkdir(parents=True)
        notices = video / 'source-and-notices'
        notices.mkdir()
        video_manifest = {'version': video_receipt['version'], 'source_sha256': video_driver.SOURCE_SHA,
                          'source_url': video_driver.SOURCE_URL, 'flags': video_receipt['flags'], 'binaries': {}}
        for name, record in video_receipt['binaries'].items():
            target = video / 'bin' / name
            shutil.copy2(decoder_stage / 'bin' / name, target)
            target.chmod(0o755)
            subprocess.run(['/usr/bin/codesign', '--force', '--sign', '-', str(target)], check=True)
            subprocess.run(['/usr/bin/codesign', '--verify', '--strict', str(target)], check=True)
            video_manifest['binaries'][name] = {'build_sha256': record['sha256'],
                'signed_sha256': hashlib.sha256(target.read_bytes()).hexdigest()}
        shutil.copy2(decoder_stage / 'source.tar.xz', notices / ('ffmpeg-' + video_driver.VERSION + '.tar.xz'))
        with tarfile.open(decoder_stage / 'source.tar.xz') as archive:
            for name in ['COPYING.LGPLv2.1', 'COPYING.LGPLv3', 'LICENSE.md']:
                member = archive.getmember('ffmpeg-' + video_driver.VERSION + '/' + name)
                if not member.isfile() or member.size > 65536:
                    raise RuntimeError('Unexpected FFmpeg notice')
                (notices / name).write_bytes(archive.extractfile(member).read())
        shutil.copy2(ROOT / 'scripts/video_decoder.py', notices / 'video_decoder.py')
        shutil.copy2(ROOT / 'scripts/ghostty_probe.py', notices / 'ghostty_probe.py')
        (video / 'PROVENANCE.json').write_text(json.dumps(video_manifest, indent=2) + '\n')
        (notices / 'README.txt').write_text('FFmpeg ' + video_driver.VERSION + '\nCopyright the FFmpeg developers.\n'
            'This reduced build is LGPL 2.1 or later. No GPL/nonfree options enabled.\n'
            'The exact unmodified source archive and build driver are included here.\n'
            'Build with python3 -I -S video_decoder.py on macOS arm64 with Apple Command Line Tools.\n'
            'Use these standalone executables from a shell, or replace both with compatible builds.\n'
            'Re-sign the modified local app after replacing sealed resources.\n'
            'See COPYING.LGPLv2.1, COPYING.LGPLv3, LICENSE.md and ../PROVENANCE.json.\n')
    font_notices = contents / 'Resources/Fonts'
    font_notices.mkdir(parents=True, exist_ok=True)
    for name in ('OFL.txt', 'NERD-FONTS-LICENSE.txt', 'PROVENANCE.json'):
        shutil.copy2(ROOT / 'Resources/Fonts' / name, font_notices / name)
    manifest = {'schema_version': 1, 'source_sha256': source_sha, 'native_source_files': source_files,
                # codesign subsequently changes Mach-O bytes. A signed binary hash
                # inside its own signed resources would be self-referential.
                'unsigned_binary_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
                'helper_sha256': {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in sorted((ROOT / 'Resources/Helpers').glob('*.py'))},
                'embedded_font_sha256': hashlib.sha256((ROOT / 'Resources/Fonts/JetBrainsMonoNerdFont-Regular.ttf').read_bytes()).hexdigest(),
                'validation_profile': validation, 'staged': staged,
                'validation_instance': instance,
                'development_identity': development,
                'build_profile': profile,
                'terminal_engine': receipt.get('terminal_engine', 'vt100'),
                'terminal_core': receipt.get('terminal_core'),
                'video_decoder': video_manifest,
                'html_renderer': html_manifest,
                'scope': 'local candidate; full R0-R7 acceptance remains open'}
    (contents / 'Resources/build-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    if validation:
        (contents / 'Resources/validation-profile').write_text('synthetic only\n')
    if instance:
        (contents / 'Resources/validation-instance').write_text(instance + '\n')
    display_name = 'Terminal Workspace Development' if development else ('Terminal Workspace Validation' if validation else 'Terminal Workspace Rust')
    if instance and not development:
        display_name += ' ' + instance
    info = {
        'CFBundleIdentifier': 'dev.local.TerminalWorkspaceRust.Development' if development else (('dev.local.TerminalWorkspaceRust.Validation' + ('.v' + instance if instance else '')) if validation else 'dev.local.TerminalWorkspaceRust'),
        'CFBundleName': display_name,
        'CFBundleDisplayName': display_name,
        'CFBundleExecutable': 'terminal-workspace',
        'CFBundlePackageType': 'APPL',
        'CFBundleShortVersionString': '0.1.0',
        'CFBundleVersion': '1',
        'LSMinimumSystemVersion': '12.0',
        'NSHighResolutionCapable': True,
        'NSPrincipalClass': 'NSApplication',
    }
    with (contents / 'Info.plist').open('wb') as out:
        plistlib.dump(info, out)
    # Launching the generated .app can attach FinderInfo to its root. codesign
    # rejects that metadata on the next package. Preserve provenance/other attrs.
    for path in [bundle, *bundle.rglob('*')]:
        attrs = subprocess.check_output(['/usr/bin/xattr', str(path)], text=True).splitlines()
        for attr in ('com.apple.FinderInfo', 'com.apple.ResourceFork'):
            if attr in attrs:
                subprocess.run(['/usr/bin/xattr', '-d', attr, str(path)], check=True)
    subprocess.run(['/usr/bin/codesign', '--force', '--sign', '-', str(bundle)], check=True)
    subprocess.run(['/usr/bin/codesign', '--verify', '--strict', str(bundle)], check=True)
    archive = None
    archive_sha = None
    # A bundle receipt is sufficient for the checked installer; ZIPs are made
    # only for an explicit archive request in any packaging mode.
    if archive_requested:
        archive_name = 'Terminal Workspace Development' if development else ('Terminal Workspace Validation' if validation else 'Terminal Workspace Rust')
        archive_suffix = (' ' + (instance or bundle.parent.name)) if staged else ''
        archive = ROOT / ('dist/' + archive_name + (' Release' if release else '') + (' Staged' if staged else '') + archive_suffix + '.zip')
        archive.parent.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(archive, 'x', compression=zipfile.ZIP_DEFLATED, compresslevel=3) as out:
            for path in sorted(bundle.rglob('*')):
                if path.is_file():
                    out.write(path, arcname=str(Path(bundle.name) / path.relative_to(bundle)))
        archive_sha = driver.retention.file_digest(archive, 256 * 1024**2).hex()
        archive_marker = archive.with_name(archive.name + '.twr-archive.json')
        with archive_marker.open('x') as output:
            json.dump({'schema': 1, 'retention_managed': True, 'in_flight': True, 'archive': str(archive),
                       'archive_sha256': archive_sha, 'bundle': str(bundle),
                       'source_snapshot': receipt['source_snapshot'], 'source_sha256': source_sha,
                       'signed_binary_sha256': hashlib.sha256(executable.read_bytes()).hexdigest()}, output)
            output.write('\n')
        archive_marker.chmod(0o600)
    if staged:
        marker = bundle.parent / '.twr-retention.json'
        fd, name = tempfile.mkstemp(prefix='.twr-marker-', dir=bundle.parent)
        pending_marker = Path(name)
        with os.fdopen(fd, 'w') as output:
            json.dump({'schema': 2, 'retention_managed': True, 'in_flight': True,
                       'state': 'ready',
                       'kind': 'development' if development else ('validation' if validation else 'release'),
                       'bundle': bundle.name, 'archive': str(archive) if archive else None,
                       'archive_sha256': archive_sha,
                       'source_sha256': source_sha,
                       'source_snapshot': receipt['source_snapshot'],
                       'binary_sha256': hashlib.sha256(executable.read_bytes()).hexdigest()}, output)
            output.write('\n')
            output.flush()
            os.fsync(output.fileno())
        if (marker.is_symlink() or not marker.is_file()
                or (marker.lstat().st_dev, marker.lstat().st_ino) != (marker_info.st_dev, marker_info.st_ino)
                or driver.retention.file_digest(marker).hex() != marker_digest):
            raise RuntimeError('Stage marker changed during packaging; preserve candidate')
        os.replace(pending_marker, marker)
    print(json.dumps({'bundle': str(bundle), 'archive': str(archive) if archive else None, 'sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
                      'signed_binary_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
                      'archive_sha256': archive_sha,
                      'build_profile': profile, 'profile': 'private persistent tmux with explicit project choice', 'ephemeralTerminals': False, 'staged': staged, 'validation_instance': instance, 'bundle_id': info['CFBundleIdentifier'], 'source_sha256': manifest['source_sha256']}, indent=2))



def launch(validation=False):
    bundle = driver.BUILD / "validation/Terminal Workspace Validation.app" if validation else BUNDLE
    if not (bundle / 'Contents/MacOS/terminal-workspace').is_file():
        raise RuntimeError('Package the app first.')
    RUNTIME.mkdir(mode=0o700, parents=True, exist_ok=True)
    subprocess.run(['/usr/bin/open', '-a', str(bundle),
                    '--stdout', str(RUNTIME / 'app.log'),
                    '--stderr', str(RUNTIME / 'app.log')], check=True)
    print(json.dumps({'launchRequested': True, 'bundle': str(bundle),
                      'note': 'Close with Cmd+Q. Request success alone does not verify a visible window.'}, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['package', 'launch', 'sessions', 'attach'])
    parser.add_argument('--validation', action='store_true', help='Package a separate synthetic app and state directory')
    parser.add_argument('--release', action='store_true', help='Package the optimized native build with its own receipt')
    parser.add_argument('--staged', action='store_true', help='Package into a new staged bundle without changing a running bundle')
    parser.add_argument('--development', action='store_true', help='Stable app identity for sequential synthetic development; requires package --validation --staged')
    parser.add_argument('--archive', action='store_true', help='Also create an archive for a Development package')
    parser.add_argument('--video-stage', help='Explicit private decoder stage from scripts/video_decoder.py; package only')
    parser.add_argument('--validation-instance', help='Eight hexadecimal characters printed by a staged validation package; sessions/attach only')
    parser.add_argument('--session', help='App session ID, p- followed by 16 hex digits')
    args = parser.parse_args()
    if args.development and (args.command != 'package' or not args.validation or not args.staged):
        parser.error('--development requires package --validation --staged; close the previous Development app before opening the next')
    if args.video_stage and args.command != 'package':
        parser.error('--video-stage is for package only')
    if args.archive and args.command != 'package':
        parser.error('--archive is for package only')
    if args.release and args.command != 'package':
        parser.error('--release is for package only')
    if args.staged and args.command != 'package':
        parser.error('--staged is for package only; close the old app before opening the printed staged bundle')
    if args.validation_instance and (not args.validation or args.command not in ('sessions', 'attach') or not re.fullmatch(r'[0-9a-f]{8}', args.validation_instance)):
        parser.error('--validation-instance requires --validation and sessions/attach, with eight hexadecimal characters')
    if args.command == 'package':
        package(args.validation, args.staged, args.release, args.development, args.video_stage, args.archive)
    elif args.command == 'launch':
        launch(args.validation)
    else:
        username = ''.join(c for c in os.environ.get('USER', 'local') if c.isascii() and (c.isalnum() or c == '_'))
        directory = Path('/private/tmp') / (('terminal-workspace-validation-' if args.validation else 'terminal-workspace-native-') + username)
        if args.validation_instance:
            directory = directory.with_name(directory.name + '-' + args.validation_instance)
        if directory.is_symlink() or not directory.is_dir() or directory.stat().st_uid != os.getuid() or directory.stat().st_mode & 0o777 != 0o700:
            parser.error('App state directory unavailable or unsafe; open the app first.')
        binary = next((p for p in ['/opt/homebrew/bin/tmux', '/usr/local/bin/tmux', '/usr/bin/tmux'] if Path(p).is_file()), None)
        if not binary:
            parser.error('tmux executable not found')
        command = [binary, '-S', str(directory / 'tmux.sock')]
        if args.command == 'sessions':
            subprocess.run(command + ['list-sessions', '-F', '#{session_name}: #{session_windows} windows, #{session_attached} clients'], check=True, timeout=5)
        else:
            if not args.session or not re.fullmatch(r'p-[0-9a-f]{16}', args.session):
                parser.error('Use --session p-<16 hex digits> from sessions output')
            os.execv(binary, command + ['attach-session', '-t', args.session])


if __name__ == '__main__':
    main()
