#!/usr/bin/env python3
"""Explicit local shell/prompt probe. No personal history or projects are imported.
Runs the user's reviewed zshrc through a private ZDOTDIR, with private history,
zoxide database and Starship cache. Raw terminal evidence stays mode0600 in tmp.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import signal
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--user-shell-config', action='store_true', required=True)
    args = parser.parse_args()
    assert args.user_shell_config
    home = Path.home()
    rc = home / '.zshrc'
    prompt = home / '.config/starship.toml'
    if not rc.is_file() or not prompt.is_file():
        raise SystemExit('Expected reviewed zshrc and Starship config')
    root = Path(tempfile.mkdtemp(prefix='tw-shell-compat-', dir='/private/tmp'))
    root.chmod(0o700)
    for name in ('zdot', 'cache', 'zoxide', 'project'):
        (root/name).mkdir(mode=0o700)
    project = root/'project'
    (project/'sample.rs').write_text('fn main() {}\n')
    (project/'folder').mkdir()
    env = {'HOME':str(home), 'USER':os.environ.get('USER','synthetic'), 'LOGNAME':os.environ.get('USER','synthetic'),
           'PATH':'/usr/bin:/bin:/usr/sbin:/sbin', 'LANG':'en_US.UTF-8',
           'ZDOTDIR':str(root/'zdot'), 'HISTFILE':str(root/'history'),
           '_ZO_DATA_DIR':str(root/'zoxide'), 'STARSHIP_CONFIG':str(prompt),
           'STARSHIP_CACHE':str(root/'cache'), 'COLORTERM':'truecolor', 'TMPDIR':str(root)}
    wrapper = ('source '+str(rc)+'\nHISTFILE='+str(root/'history')+
               '\nunsetopt SHARE_HISTORY INC_APPEND_HISTORY INC_APPEND_HISTORY_TIME\nSAVEHIST=0\n'+
               'fc -p "$HISTFILE" 1000 0\n')
    (root/'zdot/.zshrc').write_text(wrapper)
    (root/'zdot/.zshrc').chmod(0o600)
    # The user's zprofile was verified empty; system login startup/path_helper is retained.
    git_env = {**env,'GIT_CONFIG_GLOBAL':'/dev/null','GIT_CONFIG_NOSYSTEM':'1'}
    subprocess.run(['/usr/bin/git','-c','core.hooksPath=/dev/null','init','-q','-b','shell-fixture',str(project)],env=git_env,check=True)
    receipts=[]
    for term in ('xterm-ghostty','tmux-256color'):
        master,slave=pty.openpty()
        import fcntl,struct,termios
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',24,100,0,0))
        process=subprocess.Popen(['/bin/zsh','-l'],cwd=project,env={**env,'TERM':term},stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
        os.close(slave)
        data=bytearray()
        def collect(seconds):
            deadline=time.monotonic()+seconds
            while time.monotonic()<deadline:
                if select.select([master],[],[],0.1)[0]:
                    try: chunk=os.read(master,65536)
                    except OSError: break
                    if not chunk: break
                    data.extend(chunk)
                    if len(data)>1024*1024: raise RuntimeError('Shell probe output limit')
        try:
            collect(3)
            os.write(master,b" printf '\\036SHELL-CHECK:%s:%s\\037\\n' $ZSH_VERSION $HISTFILE; whence -w ls; ls --color=never --icons=always; printf '\\036PROBE-END\\037\\n'\n")
            collect(3)
            os.write(master,b' false\n')
            collect(1)
            text=data.decode('utf-8','replace')
            checks={'zsh_version':'SHELL-CHECK:5.' in text,'private_history':str(root/'history') in text,
                    'ls_alias':'ls: alias' in text,'starship_branch':'shell-fixture' in text,
                    'starship_character':'❯' in text,'failure_prompt':'✗' in text,
                    'nerd_icons':any(0xe000<=ord(c)<=0xf8ff or 0xf0000<=ord(c)<=0xffffd for c in text),
                    'startup_without_errors':not any(error in text.lower() for error in ('command not found','no such file','zle:','parse error')),'eza_file':'sample.rs' in text,
                    'finished':'\x1ePROBE-END\x1f' in text}
            os.write(master,b' exit\n')
            collect(0.2)
            output=root/(term+'.pty'); output.write_bytes(data); output.chmod(0o600)
            receipts.append({'term':term,'checks':checks,'output':str(output),'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest()})
        finally:
            if process.poll() is None: os.killpg(process.pid,signal.SIGTERM)
            try: process.wait(timeout=3)
            except subprocess.TimeoutExpired: os.killpg(process.pid,signal.SIGKILL);process.wait()
            os.close(master)
    receipt={'root':str(root),'scope':'reviewed actual zshrc and Starship config; synthetic cwd/private history and caches',
             'config_hashes':{str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in (rc,prompt)},'results':receipts}
    (root/'receipt.json').write_text(json.dumps(receipt,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(receipt,ensure_ascii=False,indent=2))
    if not all(all(r['checks'].values()) for r in receipts): raise SystemExit(1)

if __name__=='__main__': main()
