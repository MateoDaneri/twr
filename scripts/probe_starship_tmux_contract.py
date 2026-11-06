#!/usr/bin/env python3
"""Exercise synthetic zsh/Starship through a private tmux server and socket."""
import json
import subprocess
import tempfile
import time
from pathlib import Path


def main():
    root = Path(tempfile.mkdtemp(prefix='twr-prompt-', dir='/private/tmp'))
    root.chmod(0o700)
    socket = root/'t.sock'
    config = root/'starship.toml'
    config.write_text('format = "$character"\n[character]\nsuccess_symbol = "[❯](bold green)"\nerror_symbol = "[✗](bold red)"\n')
    (root/'.zshrc').write_text('unsetopt SHARE_HISTORY INC_APPEND_HISTORY INC_APPEND_HISTORY_TIME\nSAVEHIST=0\nHISTFILE="'+str(root/'history')+'"\neval "$(/opt/homebrew/bin/starship init zsh)"\n')
    env = {'HOME': str(root), 'ZDOTDIR': str(root), 'PATH': '/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin',
           'TMPDIR': str(root), 'LANG': 'en_US.UTF-8', 'TERM': 'xterm-256color', 'COLORTERM': 'truecolor',
           'STARSHIP_CONFIG': str(config), 'STARSHIP_CACHE': str(root/'cache'),
           'XDG_CONFIG_HOME': str(root/'config'), 'XDG_CACHE_HOME': str(root/'cache')}
    conf = Path(__file__).resolve().parents[1]/'Resources/Fixtures/native-tmux.conf'
    base = ['/opt/homebrew/bin/tmux', '-u', '-S', str(socket)]

    def run(args):
        return subprocess.check_output(base+args, cwd=root, env=env, stderr=subprocess.PIPE, timeout=8)

    def wait_for(needle):
        end = time.monotonic()+8
        while time.monotonic() < end:
            text = run(['capture-pane', '-p', '-t', 'fixture:1.1']).decode('utf-8')
            if needle in text:
                return
            time.sleep(.05)
        raise RuntimeError('synthetic tmux prompt did not contain expected marker')

    try:
        run(['-f', str(conf), 'new-session', '-d', '-s', 'fixture', '-n', 'terminal', '-c', str(root), '/bin/zsh -l'])
        wait_for('❯')
        run(['send-keys', '-t', 'fixture:1.1', 'false', 'Enter'])
        wait_for('✗')
        run(['send-keys', '-t', 'fixture:1.1', "printf 'TWR_SYNTH_TERM:%s\\n' \"$TERM\"", 'Enter'])
        wait_for('TWR_SYNTH_TERM:tmux-256color')
        print(json.dumps({'status': 'passed', 'scope': 'synthetic zsh and Starship in private tmux',
                          'term': 'tmux-256color', 'success': '❯', 'failure': '✗'}, ensure_ascii=False))
    finally:
        if socket.exists():
            subprocess.run(base+['kill-server'], cwd=root, env=env, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=8, check=False)


if __name__ == '__main__':
    main()
