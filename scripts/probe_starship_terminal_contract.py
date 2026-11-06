#!/usr/bin/env python3
"""Compare synthetic Starship prompt output under Ghostty and tmux TERM values."""
import json
import subprocess
import tempfile
from pathlib import Path


def main():
    root = Path(tempfile.mkdtemp(prefix='twr-starship-term-', dir='/private/tmp'))
    root.chmod(0o700)
    config = root/'starship.toml'
    config.write_text('format = "$character"\n[character]\nsuccess_symbol = "[❯](bold green)"\nerror_symbol = "[✗](bold red)"\n')
    env = {'HOME': str(root), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(root),
           'XDG_CONFIG_HOME': str(root/'config'), 'XDG_CACHE_HOME': str(root/'cache'),
           'STARSHIP_CONFIG': str(config), 'STARSHIP_CACHE': str(root/'cache'),
           'STARSHIP_SHELL': 'zsh', 'LANG': 'en_US.UTF-8', 'COLORTERM': 'truecolor'}
    output = {}
    for term in ('xterm-ghostty', 'tmux-256color'):
        for status, glyph in ((0, '❯'), (1, '✗')):
            raw = subprocess.check_output(['/opt/homebrew/bin/starship', 'prompt', '--status', str(status)],
                                          cwd=root, env={**env, 'TERM': term}, timeout=8)
            if glyph.encode() not in raw:
                raise RuntimeError(f'{term} status {status} did not contain expected synthetic glyph')
            output[f'{term}:{status}'] = raw.hex()
    for status in (0, 1):
        if output[f'xterm-ghostty:{status}'] != output[f'tmux-256color:{status}']:
            raise RuntimeError(f'Synthetic prompt differs between TERM values for status {status}')
    version = subprocess.check_output(['/opt/homebrew/bin/starship', '--version'], env=env,
                                      cwd=root, timeout=8).decode().splitlines()[0]
    print(json.dumps({'starship': version, 'bytes': output}, ensure_ascii=False))


if __name__ == '__main__':
    main()
