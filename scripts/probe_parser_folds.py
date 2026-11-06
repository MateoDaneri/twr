#!/usr/bin/env python3
"""Compare supplied synthetic fold fixtures with clean Neovim and pinned Cargo grammars.

Only reads this checkout and tool binaries. Writes a new private temporary directory;
never starts user configuration, queries a live session, or removes prior evidence.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
REFERENCE = ROOT / 'Resources/Contracts/editor-parser-fold-reference.json'


def run(command, stage, name, env=None):
    with (stage / (name + '.stdout')).open('wb') as stdout, (stage / (name + '.stderr')).open('wb') as stderr:
        subprocess.run(command, check=True, timeout=45, stdout=stdout, stderr=stderr, env=env)


def main():
    reference = json.loads(REFERENCE.read_text())
    nvim = shutil.which('nvim')
    if not nvim or not Path('/usr/bin/cc').is_file():
        raise RuntimeError('This local probe needs Neovim and /usr/bin/cc')
    version = subprocess.run([nvim, '--version'], check=True, capture_output=True, text=True, timeout=5).stdout.splitlines()[0]
    if version != 'NVIM v0.12.1':
        raise RuntimeError(f'Expected recorded Neovim v0.12.1; found {version}')
    stage = Path(tempfile.mkdtemp(prefix='tw-parser-fold-oracle-', dir='/private/tmp'))
    print(stage, flush=True)
    source = (ROOT / 'crates/workspace-app/src/syntax_folds.rs').read_text()
    registry = ROOT / '.toolchain/cargo/registry/src'
    for case in reference['cases']:
        language = case['language']
        parser_version = reference['provenance']['parser_versions'][language]
        matches = list(registry.glob(f'*/tree-sitter-{language}-{parser_version}/src/parser.c'))
        if len(matches) != 1:
            raise RuntimeError(f'Expected one pinned {language} grammar in this checkout cache')
        query = re.search(r'const ' + language.upper() + r': &str = r#"(.*?)"#;', source, re.S)
        if not query:
            raise RuntimeError(f'Embedded fold query missing: {language}')
        data = query.group(1).encode()
        if hashlib.sha256(data).hexdigest() != reference['provenance']['queries'][language]['sha256']:
            raise RuntimeError(f'Embedded query differs from pinned reference: {language}')
        (stage / (language + '.scm')).write_bytes(data)
        (stage / (language + '.txt')).write_text('\n'.join(case['text']))
        grammar = matches[0].parent
        command = ['/usr/bin/cc', '-dynamiclib', '-O0', '-I', str(grammar), str(grammar / 'parser.c')]
        if (grammar / 'scanner.c').is_file():
            command.append(str(grammar / 'scanner.c'))
        command += ['-o', str(stage / (language + '.so'))]
        run(command, stage, 'compile-' + language)
    # A fixed script and only synthetic files. Paths are encoded as Lua-compatible JSON strings.
    lua = 'local root=' + json.dumps(str(stage)) + '\n' + r'''
local result={version=vim.version(),cases={}}
for _,lang in ipairs({"rust","c","lua","json"}) do
 vim.cmd("enew!")
 vim.treesitter.language.add(lang,{path=root.."/"..lang..".so"})
 local text=vim.fn.readfile(root.."/"..lang..".txt")
 vim.api.nvim_buf_set_lines(0,0,-1,false,text)
 vim.treesitter.query.set(lang,"folds",table.concat(vim.fn.readfile(root.."/"..lang..".scm"),"\n"))
 vim.treesitter.start(0,lang)
 vim.wo.foldminlines=1;vim.wo.foldnestmax=20;vim.wo.foldlevel=99
 vim.wo.foldmethod="expr";vim.wo.foldexpr="v:lua.vim.treesitter.foldexpr()"
 vim.treesitter.get_parser(0,lang):parse()
 vim.cmd("normal! zx")
 assert(vim.wait(1000,function()return vim.fn.foldlevel(1)>0 end),"Fold oracle did not become ready")
 local levels={};for row=1,#text do levels[row]=vim.fn.foldlevel(row) end
 vim.cmd("normal! zM")
 local closed={};for row=1,#text do closed[row]={vim.fn.foldclosed(row),vim.fn.foldclosedend(row)} end
 result.cases[#result.cases+1]={language=lang,text=text,levels=levels,closed=closed}
end
vim.fn.writefile({vim.json.encode(result)},root.."/actual.json")
vim.cmd("qa!")
'''
    script = stage / 'oracle.lua'
    script.write_text(lua)
    env = os.environ.copy()
    env['NVIM_LOG_FILE'] = str(stage / 'nvim.log')
    run([nvim, '--clean', '-n', '-i', 'NONE', '--headless', '-l', str(script)], stage, 'nvim', env)
    actual = json.loads((stage / 'actual.json').read_text())
    expected = {case['language']: case for case in reference['cases']}
    if len(actual['cases']) != len(expected):
        raise RuntimeError('Incomplete oracle result')
    for case in actual['cases']:
        if case != expected[case['language']]:
            raise RuntimeError(f'Oracle differs from reference: {case["language"]}; evidence kept at {stage}')
    (stage / 'receipt.json').write_text(json.dumps({'status': 'passed', 'nvim': version, 'languages': list(expected), 'reference_sha256': hashlib.sha256(REFERENCE.read_bytes()).hexdigest()}, indent=2) + '\n')
    print('PASS: all four language levels and closed ranges match; logs and actual.json retained', flush=True)


if __name__ == '__main__':
    main()
