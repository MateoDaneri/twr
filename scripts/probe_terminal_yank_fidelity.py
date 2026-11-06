#!/usr/bin/env python3
"""Compare synthetic tmux capture and Neovim terminal yanks without user state."""
import json
import subprocess
import tempfile
import time
from pathlib import Path

stage = Path(tempfile.mkdtemp(prefix='tw-yank-fidelity-', dir='/private/tmp'))
stage.chmod(0o700)
tmux = ['/opt/homebrew/bin/tmux', '-S', str(stage/'socket'), '-f', '/dev/null']
fixture = "printf 'written   \\nabcdef\\rxy\\033[K\\nabcdef\\rxy\\033[4X\\n'; exec /bin/cat"
def run_tmux(*args):
    return subprocess.run(tmux + list(args), stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          check=True, timeout=5).stdout.decode()
try:
    run_tmux('new-session', '-d', '-s', 'synthetic', '-x', '40', '-y', '10', fixture)
    deadline = time.monotonic() + 5
    while True:
        capture = run_tmux('capture-pane', '-pNT', '-t', 'synthetic:', '-S', '0', '-E', '2')
        if capture.startswith('written   \nxy') and capture.count('xy') == 2:
            break
        if time.monotonic() >= deadline:
            raise RuntimeError('Synthetic terminal did not reach expected content')
        time.sleep(.02)
    lua = stage/'reference.lua'
    lua.write_text('''vim.opt.columns=40; vim.opt.lines=10
vim.fn.termopen({'/bin/sh','-c',"printf 'written   \\\\nabcdef\\\\rxy\\\\033[K\\\\nabcdef\\\\rxy\\\\033[4X\\\\n'; exec /bin/cat"})
assert(vim.wait(2000, function() local l=vim.api.nvim_buf_get_lines(0,0,3,false); return #l==3 and l[1]:match('written') and l[3]:match('xy') end, 10), 'terminal output timeout')
local lines=vim.api.nvim_buf_get_lines(0,0,3,false); local yanks={}
for i=1,3 do vim.api.nvim_win_set_cursor(0,{i,0}); vim.cmd('normal! yy'); table.insert(yanks,vim.fn.getreg('"')) end
io.stdout:write(vim.json.encode({lines=lines,yanks=yanks}),'\\n'); vim.cmd('qa!')
''')
    env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
           'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_STATE_HOME': str(stage/'state'),
           'XDG_DATA_HOME': str(stage/'data'), 'XDG_CACHE_HOME': str(stage/'cache'),
           'NVIM_LOG_FILE': str(stage/'nvim.log')}
    reference = json.loads(subprocess.check_output(['/opt/homebrew/bin/nvim', '--headless', '-u', 'NONE',
                           '-i', 'NONE', '--noplugin', '-n', '-l', str(lua)], cwd=stage,
                           env=env, stderr=subprocess.PIPE, timeout=10))
    expected = ['written   \n', 'xy\n', 'xy\n']
    if reference['yanks'] != expected or capture.splitlines() != ['written   ', 'xy    ', 'xy    ']:
        raise RuntimeError('Observed behavior differs; inspect fixture and versions before using this reference')
    nvim_version = subprocess.check_output(['/opt/homebrew/bin/nvim', '--version'], cwd=stage,
                                           env=env, timeout=5).decode().splitlines()[0]
    result = {'tmux_version': run_tmux('-V').strip(), 'nvim_version': nvim_version, 'capture_pNT': capture,
              'nvim_yanks': reference['yanks'], 'nvim_lines': reference['lines']}
    (stage/'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps({'stage': str(stage), **result}, ensure_ascii=False))
finally:
    try:
        run_tmux('kill-server')
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
        pass
