#!/usr/bin/env python3
"""Observe window-view recall across synthetic buffers, with clean Neovim RPC."""
import argparse
import json
import socket as unix_socket
import threading,select
import subprocess
import tempfile
import time
from pathlib import Path


UNICODE_LINE = '界'*80 + '\t' + 'e\u0301'*40 + 'XYZ' + '\t' + 'a'*100
UNICODE_CASES = [
    ('wide-prefix', [('a',50,161,'zz'),('b',60,0,'zz'),('a',None,None,None)]),
    ('mixed-prefix', [('a',50,195,'zz'),('b',60,161,'zz'),('a',None,None,None),('b',None,None,None)]),
]

CASES = [
    ('horizontal-and-vertical', [('a',50,120,'zz'),('b',100,2,'zt'),('a',None,None,None),('b',None,None,None),('b',None,None,None)]),
    ('same-buffer', [('a',50,50,'zt'),('a',None,None,None)]),
    ('nearby-buffers', [('a',50,50,'zt'),('b',55,120,'zz'),('a',None,None,None),('b',None,None,None)]),
    ('both-scrolled', [('a',100,90,'zz'),('b',50,120,'zz'),('a',None,None,None),('b',None,None,None)]),
    ('right-below-half', [('a',50,110,'zz'),('b',60,0,'zz'),('a',None,None,None)]),
    ('right-at-half', [('a',50,111,'zz'),('b',60,0,'zz'),('a',None,None,None)]),
    ('right-above-half', [('a',50,112,'zz'),('b',60,0,'zz'),('a',None,None,None)]),
    ('left-at-half', [('a',50,48,'zz'),('b',60,120,'zz'),('a',None,None,None)]),
    ('left-above-half', [('a',50,47,'zz'),('b',60,120,'zz'),('a',None,None,None)]),
    ('near-start', [('a',3,2,'zt'),('b',60,120,'zz'),('a',None,None,None)]),
    ('near-end', [('a',190,90,'zb'),('b',20,5,'zz'),('a',None,None,None),('b',None,None,None)]),
]


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--record',action='store_true',help='Intentionally replace the verified reference fixture')
    parser.add_argument('--unicode',action='store_true',help='Use synthetic mixed-width text and scalar-to-byte cursor conversion')
    args=parser.parse_args()
    line=UNICODE_LINE if args.unicode else 'a'*160
    cases=UNICODE_CASES if args.unicode else CASES
    stage=Path(tempfile.mkdtemp(prefix='tw-buffer-view-reference-',dir='/private/tmp'));stage.chmod(0o700)
    binary='/opt/homebrew/bin/nvim'
    env={'HOME':str(stage),'TMPDIR':str(stage),'PATH':'/usr/bin:/bin',
         'XDG_CONFIG_HOME':str(stage/'config'),'XDG_DATA_HOME':str(stage/'data'),
         'XDG_STATE_HOME':str(stage/'state'),'XDG_CACHE_HOME':str(stage/'cache'),
         'NVIM_LOG_FILE':str(stage/'nvim.log')}
    lua=r'''
_G.tw_setup=function()
 vim.cmd('enew!')
 _G.buffers={a=vim.api.nvim_get_current_buf(),b=vim.api.nvim_create_buf(true,false)}
 for _,id in pairs(buffers) do
  local lines={};for i=1,200 do table.insert(lines,REPLACEMENT_LINE) end
  vim.api.nvim_buf_set_lines(id,0,-1,false,lines)
 end
 return true
end
_G.tw_visit=function(name,row,column,scroll)
 vim.api.nvim_set_current_buf(buffers[name])
 vim.cmd('redraw!');local _=vim.fn.winline();_=vim.fn.wincol()
 local entry=vim.fn.winsaveview()
 if row then vim.api.nvim_win_set_cursor(0,{row,column}) end
 if scroll then vim.cmd('normal! '..scroll) end
 vim.cmd('redraw!');local _=vim.fn.winline();_=vim.fn.wincol()
 local view=vim.fn.winsaveview()
 assert(view.lnum-1>=view.topline-1 and view.lnum-1<view.topline-1+vim.api.nvim_win_get_height(0),'Cursor outside vertical viewport')
 assert(vim.fn.virtcol('.')-1>=view.leftcol and vim.fn.virtcol('.')-1<view.leftcol+vim.api.nvim_win_get_width(0),'Cursor outside horizontal viewport')
 return {virtual_column=vim.fn.virtcol('.')-1,buffer=name,row=view.lnum-1,column=view.col,top=view.topline-1,left=view.leftcol,entry_top=entry.topline-1,entry_left=entry.leftcol,entry_row=entry.lnum-1,entry_column=entry.col,height=vim.api.nvim_win_get_height(0),width=vim.api.nvim_win_get_width(0)}
end
'''
    lua=lua.replace('REPLACEMENT_LINE',json.dumps(line,ensure_ascii=False))
    if not args.unicode: lua=lua.replace("virtual_column=vim.fn.virtcol('.')-1,",'')
    (stage/'setup.lua').write_text(lua)
    socket=stage/'rpc.sock'
    process=subprocess.Popen([binary,'--headless','-u','NONE','-i','NONE','--noplugin','-n',
        '--listen',str(socket),'-c','set hidden nowrap scrolloff=4 sidescrolloff=8 sidescroll=1 splitkeep=screen tabstop=2',
        '-c','luafile '+str(stage/'setup.lua')],env=env,cwd=stage,stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,stderr=(stage/'stderr.log').open('wb'))
    ui=None;reader=None;stopped=threading.Event();transcript=bytearray()
    # Fixed tiny request, per msgpack/msgpack and msgpack-rpc primary specs.
    # Only constants are encoded; this is not a general protocol client/decoder.
    def pack(value):
        if type(value) is bool: return b'\xc3' if value else b'\xc2'
        if type(value) is int and 0<=value<128: return bytes([value])
        if type(value) is str and len(value.encode())<32:
            data=value.encode();return bytes([0xa0+len(data)])+data
        if type(value) is list and len(value)<16:
            return bytes([0x90+len(value)])+b''.join(pack(item) for item in value)
        if type(value) is dict and len(value)<16:
            return bytes([0x80+len(value)])+b''.join(pack(key)+pack(item) for key,item in value.items())
        raise ValueError('Outside fixed UI attach request subset')
    def drain():
        while not stopped.is_set():
            try:
                ready,_,_=select.select([ui],[],[],.05)
                if ready:
                    data=ui.recv(65536)
                    if not data: break
                    if len(transcript)<2*1024*1024: transcript.extend(data[:2*1024*1024-len(transcript)])
            except OSError: break
    deadline=time.monotonic()+30
    def remote(*args):
        if time.monotonic()>deadline: raise TimeoutError('Reference overall deadline')
        return subprocess.check_output([binary,'--headless','--server',str(socket),*args],env=env,cwd=stage,stderr=subprocess.PIPE,timeout=3).decode()
    results=[]
    try:
        startup=time.monotonic()+5
        while not socket.exists():
            if time.monotonic()>startup or process.poll() is not None: raise RuntimeError('Clean reference failed to start: '+str(stage))
            time.sleep(.01)
        ui=unix_socket.socket(unix_socket.AF_UNIX,unix_socket.SOCK_STREAM);ui.settimeout(3);ui.connect(str(socket))
        ui.sendall(pack([0,1,'nvim_ui_attach',[80,24,{'rgb':True,'ext_linegrid':True}]]))
        reader=threading.Thread(target=drain,daemon=True);reader.start()
        while True:
            uis=json.loads(remote('--remote-expr','json_encode(nvim_list_uis())'))
            if any(item['width']==80 and item['height']==24 for item in uis): break
            if time.monotonic()>startup: raise RuntimeError('Own UI failed to attach: '+str(stage))
            time.sleep(.02)
        for label,visits in cases:
            remote('--remote-expr',"luaeval('tw_setup()')")
            observations=[]
            for name,row,column,scroll in visits:
                # All values come from the fixed synthetic case table.
                rpc_column=len(line[:column].encode()) if args.unicode and column is not None else column
                visit_args=','.join('nil' if x is None else json.dumps(x) for x in [name,row,rpc_column,scroll])
                observation=json.loads(remote('--remote-expr',"json_encode(luaeval('tw_visit("+visit_args+")'))"))
                if args.unicode:
                    for key in ['column','entry_column']:
                        observation[key]=len(line.encode()[:observation[key]].decode())
                observations.append(observation)
            results.append({'label':label,'visits':[list(visit) for visit in visits],'observations':observations})
        version=subprocess.check_output([binary,'--version'],env=env,cwd=stage,timeout=3).decode().splitlines()[0]
        data={'version':version,'config':'NONE','plugins':False,'scope':'One headless Neovim with an external RPC UI grid80x24 and settled viewport queries; no Ghostty GUI',
              'options':{'hidden':True,'wrap':False,'scrolloff':4,'sidescrolloff':8,'sidescroll':1,'splitkeep':'screen'},'cases':results}
        output=stage/'results.json';output.write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n')
        if args.unicode: data['line']=line;data['options']['tabstop']=2;data['column_units']='Unicode scalar index except virtual_column/left in display cells'
        baseline=Path(__file__).resolve().parents[1]/'Resources/Fixtures'/('nvim-buffer-columns.json' if args.unicode else 'nvim-buffer-views.json')
        if args.record:
            if baseline.is_symlink(): raise RuntimeError('Refuse symlink fixture')
            baseline.write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n')
        if baseline.exists() and json.loads(baseline.read_text())!=data: raise RuntimeError('Reference differs, evidence '+str(output))
        print(json.dumps({'reference':str(output),'cases':len(results),'baseline_compared':baseline.exists() and not args.record,'recorded':args.record}))
    finally:
        try: remote('--remote-send','<Esc>:qa!<CR>')
        except Exception: pass
        try: process.wait(timeout=3)
        except subprocess.TimeoutExpired: process.kill();process.wait()
        stopped.set()
        if ui is not None: ui.close()
        if reader is not None: reader.join(timeout=1)
        (stage/'ui-msgpack.bin').write_bytes(transcript)


if __name__=='__main__': main()
