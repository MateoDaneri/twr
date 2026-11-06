#!/usr/bin/env python3
"""Capture clean Neovim case operators on a synthetic closed fold."""
import argparse
import json
import subprocess
import tempfile
from pathlib import Path

CASES = [('jgUgU', True), ('j2gUgU', True), ('jgUU', True), ('j2gUU', True),
         ('jg~g~', True), ('j2g~~', True), ('j2gUgU', False), ('j2gUU', False)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record', action='store_true')
    args = parser.parse_args()
    stage = Path(tempfile.mkdtemp(prefix='twr-case-fold-', dir='/private/tmp'))
    stage.chmod(0o700)
    env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
           'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_STATE_HOME': str(stage/'state'),
           'XDG_DATA_HOME': str(stage/'data'), 'XDG_CACHE_HOME': str(stage/'cache'),
           'NVIM_LOG_FILE': str(stage/'nvim.log')}
    cases = []
    for keys, tail in CASES:
        lines = ['first', 'fn demo() {', '  x();', '}'] + (['last'] if tail else [])
        lua = ("lua vim.api.nvim_buf_set_lines(0,0,-1,false,{" + ','.join(map(json.dumps, lines)) + "}); "
               "vim.cmd('set foldmethod=manual'); vim.cmd('2,4fold'); "
               "local closed=vim.fn.foldclosed(2); "
               "pcall(vim.cmd.normal,{args={" + json.dumps(keys) + "},bang=true}); "
               "io.write(vim.json.encode({lines=vim.api.nvim_buf_get_lines(0,0,-1,false),"
               "cursor=vim.api.nvim_win_get_cursor(0),closed=closed}))")
        output = subprocess.check_output(['/opt/homebrew/bin/nvim', '--headless', '-u', 'NONE', '-i',
                                          'NONE', '--noplugin', '-n', '-c', lua, '-c', 'qa!'],
                                         env=env, cwd=stage, stderr=subprocess.PIPE, timeout=8)
        cases.append({'keys': keys, 'input': lines, **json.loads(output)})
    fixture = Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-case-line-fold.json'
    data = {'cases': cases}
    if args.record:
        if fixture.is_symlink():
            raise RuntimeError('Refusing symlink fixture')
        fixture.write_text(json.dumps(data, separators=(',', ':')) + '\n')
    elif json.loads(fixture.read_text()) != data:
        raise RuntimeError('Neovim case-fold reference differs')
    print(json.dumps({'cases': len(cases), 'recorded': args.record}))


if __name__ == '__main__':
    main()
