#!/usr/bin/env python3
"""Native Seatbelt checks using only owned fixtures and a loopback listener.

Run from the synthetic app terminal. A nested agent shell can itself prohibit
sandbox_init; that failure must not be confused with a renderer result.
"""
import importlib.util
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('render_preview', ROOT / 'Resources/Helpers/render_preview.py')
renderer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(renderer)


def main(browser=False):
    stage = Path(tempfile.mkdtemp(prefix='tw-tex-confinement-', dir='/private/tmp'))
    project, output, cache, forbidden = [stage / name for name in ['project', 'output', 'cache', 'forbidden']]
    for directory in [project, output, cache, forbidden]:
        directory.mkdir(mode=0o700)
    for directory in [project, cache, forbidden]:
        (directory / 'fixture').write_text('owned fixture\n')
    (project / 'linked-fixture').symlink_to(forbidden / 'fixture')
    profile = renderer.sandbox_profile(output, project, [Path('/bin/sh')], cache, browser=browser)
    env = {'HOME': str(output), 'TMPDIR': str(output), 'PATH': '/usr/bin:/bin', 'LANG': 'en_US.UTF-8'}
    results = []

    def run(label, command, allowed, sandbox=True, permission_detail=True):
        args = ['/usr/bin/sandbox-exec', '-p', profile, *command] if sandbox else command
        result = subprocess.run(args, cwd=output, env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=5)
        report = {'check': label, 'returncode': result.returncode, 'stdout': result.stdout.decode(errors='replace')[:2048], 'stderr': result.stderr.decode(errors='replace')[:2048]}
        results.append(report)
        assert (result.returncode == 0) == allowed, report
        if not allowed:
            assert result.returncode == 1, report
            if permission_detail:
                assert 'Operation not permitted' in report['stderr'], report

    try:
        run('read-project', ['/bin/cat', str(project / 'fixture')], True)
        run('read-cache', ['/bin/cat', str(cache / 'fixture')], True)
        run('deny-unrelated-fixture', ['/bin/cat', str(forbidden / 'fixture')], False)
        run('deny-symlink-escape', ['/bin/cat', str(project / 'linked-fixture')], False)
        write = ['/bin/sh', '-c', 'printf owned > "$1"', 'probe']
        run('write-output', [*write, str(output / 'created')], True)
        run('deny-project-write', [*write, str(project / 'created')], False)
        run('deny-cache-write', [*write, str(cache / 'created')], False)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            listener.listen(2)
            command = ['/usr/bin/nc', '-v', '-z', '-w', '1', '127.0.0.1', str(listener.getsockname()[1])]
            run('loopback-control', command, True, sandbox=False)
            run('deny-loopback-network', command, False)
        if browser:
            for directory, allowed in [(output, True), (forbidden, False)]:
                with socket.socket(socket.AF_UNIX) as listener:
                    path = directory / 'probe.sock'
                    listener.bind(str(path))
                    listener.listen(2)
                    command = ['/usr/bin/nc', '-v', '-U', '-w', '1', str(path)]
                    run('unix-control-' + directory.name, command, True, sandbox=False)
                    # macOS nc reports Unix connection failure silently even
                    # with -v. The unsandboxed control uses the same live path.
                    run('unix-confined-' + directory.name, command, allowed, permission_detail=False)
        assert (output / 'created').read_text() == 'owned'
        assert not (project / 'created').exists() and not (cache / 'created').exists()
        control, managed = output / 'control-child', output / 'managed-child'
        command = ['/bin/sh', '-c', '(sleep 2; printf orphan > "$1") & exit 0', 'probe']
        subprocess.run([*command, str(control)], cwd=output, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True, check=True, timeout=5)
        renderer.run([*command, str(managed)], output, project, [Path('/bin/sh')], cache)
        time.sleep(2.4)
        assert control.read_text() == 'orphan'
        assert not managed.exists(), 'Renderer left its owned descendant alive'
        results.extend([{'check': 'background-child-control', 'marker_created': True}, {'check': 'owned-group-reaped-after-leader-exit', 'marker_created': False}])
    finally:
        (stage / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
        print(stage)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--browser', action='store_true', help='Also check render-output-only Unix IPC and denied TCP/IP')
    main(parser.parse_args().browser)
