#!/usr/bin/env python3
"""Capture clean Neovim repeated g-case operators on synthetic text."""
import argparse
import json
import subprocess
import tempfile
from pathlib import Path

CASES = ['g~g~', 'gUgU', 'gugu', '2gUgU', 'gU2gU', '2gU2gU', 'jlg~g~',
         '99g~g~', 'G99gUgU', 'Gk99gUgU', '2jgUgU', '2jgugu',
         'G99gUU', 'G99guu', 'G99g~~', 'Gk99gUU', '99gUU']
REPEAT_CASES = ['gUgUj.', 'g~g~j.', '2gUgU2j.', 'gugu2j.']
FAILED_CASES = [['g~g~', 'G99gUgU', 'gg', '.'], ['g~g~', 'G99gUgU', '.']]
GOTO_CASES = ['jlgUG', 'jlgU1G', 'jl1gUG', 'jlgu1G', 'jl1guG', 'jlg~1G', 'jl1g~G',
              '4jlgU2G', '4jl2gUG', '4jl2gU2G', 'jlgU3G', 'jlgU99G', 'jlgugG']
LINES = ['  AbC', 'dÉf', 'ñÉß', 'jkL', 'mNo', 'pQr']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record', action='store_true')
    parser.add_argument('--repeat', action='store_true')
    parser.add_argument('--after-failed', action='store_true')
    parser.add_argument('--goto', action='store_true')
    args = parser.parse_args()
    if sum((args.repeat, args.after_failed, args.goto)) > 1:
        parser.error('--repeat, --after-failed and --goto are mutually exclusive')
    stage = Path(tempfile.mkdtemp(prefix='twr-case-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    env = {'HOME': str(stage), 'PATH': '/usr/bin:/bin', 'TMPDIR': str(stage),
           'XDG_CONFIG_HOME': str(stage/'config'), 'XDG_STATE_HOME': str(stage/'state'),
           'XDG_DATA_HOME': str(stage/'data'), 'XDG_CACHE_HOME': str(stage/'cache'),
           'NVIM_LOG_FILE': str(stage/'nvim.log')}
    results = []
    for entry in (FAILED_CASES if args.after_failed else REPEAT_CASES if args.repeat else GOTO_CASES if args.goto else CASES):
        chunks = entry if args.after_failed else [entry]
        sequence = ''.join(chunks)
        operations = '; '.join('pcall(vim.cmd.normal,{args={' + json.dumps(chunk) + '},bang=true})' for chunk in chunks)
        lua = ("lua vim.api.nvim_buf_set_lines(0,0,-1,false," + ("{" + ",".join(json.dumps(line, ensure_ascii=False) for line in LINES) + "}") + "); "
               + operations + "; "
               "io.write(vim.json.encode({lines=vim.api.nvim_buf_get_lines(0,0,-1,false),"
               "cursor=vim.api.nvim_win_get_cursor(0)}))")
        output = subprocess.check_output(['/opt/homebrew/bin/nvim', '--headless', '-u', 'NONE',
                                          '-i', 'NONE', '--noplugin', '-n', '-c', lua,
                                          '-c', 'qa!'], env=env, cwd=stage,
                                         stderr=subprocess.PIPE, timeout=8)
        results.append({'keys': sequence, **json.loads(output)})
    reference = {'lines': LINES, 'cases': results}
    fixture_name = ('nvim-case-line-goto.json' if args.goto else 'nvim-case-line-after-failed.json' if args.after_failed else
                    'nvim-case-line-repeat.json' if args.repeat else 'nvim-case-line-operator.json')
    fixture = Path(__file__).resolve().parents[1]/'Resources/Fixtures'/fixture_name
    if args.record:
        if fixture.is_symlink():
            raise RuntimeError('Refusing symlink fixture')
        fixture.write_text(json.dumps(reference, ensure_ascii=False, separators=(',', ':')) + '\n')
    elif json.loads(fixture.read_text()) != reference:
        raise RuntimeError('Neovim case operator reference differs')
    print(json.dumps({'cases': len(results), 'recorded': args.record}))


if __name__ == '__main__':
    main()
