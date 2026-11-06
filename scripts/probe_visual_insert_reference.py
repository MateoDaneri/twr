#!/usr/bin/env python3
"""Clean Neovim reference for Insert word deletion and Visual window changes.
Synthetic HOME/XDG/buffers only; no personal config, plugins or shada.
"""
import json
from pathlib import Path
import subprocess
import tempfile
import time

CASES = [
    ('word', 'foo bar', 7), ('spaces', 'foo bar   ', 10),
    ('punctuation', 'foo.bar', 7), ('punctuation-tail', 'foo...', 6),
    ('underscore', 'foo_bar', 7), ('unicode', 'foo ñÁfono', 10),
    ('combining', 'foo e\u0301', 6), ('emoji', 'foo 😀😀', 6),
    ('cjk', 'foo 中文', 6), ('indent', '    foo', 7),
    ('line-start', 'first\nnext', 6), ('empty-line', 'first\n', 6),
    ('buffer-start', 'foo', 0), ('middle', 'foo bar tail', 7),
    ('tabs', 'foo\t\t', 5),
]

def main():
    stage = Path(tempfile.mkdtemp(prefix='tw-visual-insert-reference-', dir='/private/tmp'))
    stage.chmod(0o700)
    env = {'HOME': str(stage), 'TMPDIR': str(stage), 'PATH':'/usr/bin:/bin',
           'NVIM_LOG_FILE':str(stage/'nvim.log'),
           **{f'XDG_{n}_HOME':str(stage/n.lower()) for n in ('CONFIG','STATE','DATA','CACHE')}}
    binary='/opt/homebrew/bin/nvim'; socket=stage/'rpc.sock'
    process=subprocess.Popen([binary,'--headless','-u','NONE','-i','NONE','--noplugin','-n','--listen',str(socket)],env=env,cwd=stage,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    def remote(*args):
        return subprocess.check_output([binary,'--server',str(socket),*args],env=env,cwd=stage,stderr=None,timeout=4).decode()
    def lua(source):
        return remote('--remote-expr', 'luaeval('+json.dumps('(function() '+source+' end)()',ensure_ascii=False)+')')
    def observe():
        return json.loads(remote('--remote-expr',"json_encode({'mode':mode(),'lines':getline(1,'$'),'position':getcurpos()[1:2],'buffer':bufname('%')})"))
    result={'scope':'clean Neovim defaults; no personal config/plugins/shada','insert':[],'visual':[]}
    try:
        deadline=time.monotonic()+5
        while not socket.exists():
            if time.monotonic()>deadline or process.poll() is not None: raise RuntimeError(str(stage))
            time.sleep(.01)
        for label,text,cursor in CASES:
            remote('--remote-send','<Esc>')
            row=text[:cursor].count('\n')+1
            col=len(text[:cursor].rsplit('\n',1)[-1].encode())
            lua('vim.cmd("enew!"); vim.api.nvim_buf_set_lines(0,0,-1,false,'+('{'+','.join(json.dumps(line,ensure_ascii=False) for line in text.split('\n'))+'}')+'); vim.api.nvim_win_set_cursor(0,{'+str(row)+','+str(col)+'}); return true')
            remote('--remote-send','a' if col else 'i')
            # Enter Insert at the exact requested byte position, including EOL.
            lua('vim.api.nvim_win_set_cursor(0,{'+str(row)+','+str(col)+'}); return true')
            remote('--remote-send','<C-w>'); time.sleep(.02)
            result['insert'].append({'label':label,'text':text,'cursor':cursor,**observe()})
        for prefix in ('v','V','<C-v>'):
            for direction in ('h','l'):
                remote('--remote-send','<Esc>')
                remote('--remote-expr',"execute('silent! %bwipeout! | enew | file content | rightbelow vnew | file tree | wincmd h')")
                remote('--remote-send',prefix+'<C-w>'+direction); time.sleep(.02)
                result['visual'].append({'prefix':prefix,'direction':direction,**observe()})
        result['version']=subprocess.check_output([binary,'--version'],env=env,timeout=4).decode().splitlines()[0]
        path=stage/'result.json'; path.write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
        baseline = json.loads((Path(__file__).resolve().parents[1]/'Resources/Fixtures/nvim-visual-insert.json').read_text())
        assert result['insert'] == baseline['insert'] and result['visual'] == baseline['visual'], str(path)
        print(json.dumps({'receipt':str(path),'insert':result['insert'],'visual':result['visual']},ensure_ascii=False))
    finally:
        try: remote('--remote-send','<Esc>:qa!<CR>'); process.wait(timeout=3)
        except (OSError,subprocess.SubprocessError): process.terminate(); process.wait(timeout=3)
if __name__=='__main__': main()
