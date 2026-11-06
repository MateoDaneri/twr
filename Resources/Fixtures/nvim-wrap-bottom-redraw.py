"""Capture synthetic Neovim recenter behavior with a real UI attached.

All startup and files are isolated in a new private directory under /private/tmp.
This is reference data, not a test of the user's configuration or TWR GUI.
Arguments are width, height, scrolloff, tabstop, optionally followed by 'ascii'.
"""
import json, select, socket, subprocess, tempfile, threading, time, sys
width,height,margin,tabstop = map(int, sys.argv[1:5])
requested_columns = (0,)
from pathlib import Path


def pack(value):
 if type(value) is bool: return b'\xc3' if value else b'\xc2'
 if type(value) is int and 0 <= value < 128: return bytes([value])
 if type(value) is str and len(value.encode()) < 32:
  encoded=value.encode(); return bytes([0xa0 + len(encoded)]) + encoded
 if type(value) is list and len(value) < 16:
  return bytes([0x90 + len(value)]) + b''.join(map(pack, value))
 if type(value) is dict and len(value) < 16:
  return bytes([0x80 + len(value)]) + b''.join(pack(k) + pack(v) for k,v in value.items())
 raise ValueError('Unsupported UI attachment message')
stage=Path(tempfile.mkdtemp(prefix='twr-zz-ref-',dir='/private/tmp')); stage.chmod(0o700)
rpc=stage/'rpc.sock'
env={'HOME':str(stage),'PATH':'/usr/bin:/bin','TMPDIR':str(stage),'XDG_CONFIG_HOME':str(stage/'config'),'XDG_STATE_HOME':str(stage/'state'),'XDG_DATA_HOME':str(stage/'data'),'XDG_CACHE_HOME':str(stage/'cache'),'NVIM_LOG_FILE':str(stage/'nvim.log')}
version=subprocess.check_output(['/opt/homebrew/bin/nvim','--version'],env=env,cwd=stage,text=True).splitlines()[0]
if version != 'NVIM v0.12.1': raise RuntimeError('Reference requires Neovim 0.12.1, got '+version)
process=subprocess.Popen(['/opt/homebrew/bin/nvim','--headless','-u','NONE','-i','NONE','--noplugin','-n','--listen',str(rpc)],env=env,cwd=stage,stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
ui=None; stop=threading.Event(); worker=None
try:
 deadline=time.monotonic()+8
 while not rpc.exists():
  if time.monotonic()>deadline or process.poll() is not None: raise RuntimeError('startup failed')
  time.sleep(.01)
 ui=socket.socket(socket.AF_UNIX);ui.connect(str(rpc));ui.sendall(pack([0,1,'nvim_ui_attach',[width,height,{'rgb':True,'ext_linegrid':True}]]))
 def drain():
  while not stop.is_set():
   try:
    if select.select([ui],[],[],.1)[0] and not ui.recv(65536): break
   except OSError: break
 worker=threading.Thread(target=drain,daemon=True);worker.start()
 def remote(expr):
  return subprocess.check_output(['/opt/homebrew/bin/nvim','--server',str(rpc),'--remote-expr',expr],env=env,cwd=stage,stderr=subprocess.PIPE,timeout=5).decode()
 remote("execute('set wrap linebreak smoothscroll nonumber norelativenumber signcolumn=no tabstop="+str(tabstop)+" scrolljump=1 scrolloff="+str(margin)+"')")
 if sys.argv[5:] == ['ascii']:
  lines=[('line%02d '%i)+('x'*((i%3)*30)) for i in range(1,31)];lines[9]='L'*600
 else:
  lines=[('line%02d '%i)+('añ界\tZ '*((i%3)*5)) for i in range(1,31)];lines[9]='añ界\tZ '*100
 remote("luaeval('vim.api.nvim_buf_set_lines(0,0,-1,false,_A)',"+json.dumps(lines,ensure_ascii=False)+")")
 if json.loads(remote("json_encode(getline(1,'$'))")) != lines: raise RuntimeError('reference text changed')
 rows=int(remote('winheight(0)')); columns=int(remote('winwidth(0)')); cases=[]
 if (rows,columns)!=(height-2,width): raise RuntimeError('Unexpected Neovim UI geometry')
 for ln in (3,):
  for requested in dict.fromkeys(min(n,len(lines[ln-1])-1) for n in requested_columns):
   for command in ('zb',):
    for forced_top in [(1,0),(1,1),(1,2),(2,0),(2,1),(2,2),(2,3),(2,4),(2,5),(3,0)]:
     scalar=min(requested,len(lines[ln-1])-1); col=len(lines[ln-1][:scalar].encode())
     remote("execute('normal! gg0')")
     remote("luaeval('vim.api.nvim_win_set_cursor(0,_A)',"+json.dumps([ln,col])+")")
     remote("winrestview({'lnum':3,'col':0,'topline':"+str(forced_top[0])+",'skipcol':"+str(forced_top[1]*width)+"})")
     remote("screenpos(0,line('.'),col('.'))")
     before=json.loads(remote('json_encode(winsaveview())'))
     instant=json.loads(remote("json_encode(luaeval('(function() vim.cmd(\"normal! zb\"); return vim.fn.winsaveview() end)()'))"))
     remote("screenpos(0,line('.'),col('.'))")
     after=json.loads(remote('json_encode(winsaveview())'))
     cursor=json.loads(remote('json_encode(getcurpos())'))
     corrected_line=cursor[1]; corrected_scalar=len(lines[corrected_line-1].encode()[:cursor[2]-1].decode()); cases.append({'instant':[instant['topline'],instant['skipcol']],'forced':forced_top,'command':command,'line':ln,'col':scalar,'before':[before['topline'],before['skipcol']],'after':[after['topline'],after['skipcol']],'cursor':[corrected_line,corrected_scalar],'cursor_coladd':cursor[3],'row':int(remote("winline()"))})
 print(json.dumps({'rows':rows,'columns':columns,'scrolloff':margin,'tabstop':tabstop,'cases':cases},sort_keys=True))
finally:
 stop.set()
 if ui: ui.close()
 if worker: worker.join(timeout=1)
 if process.poll() is None:
  try: subprocess.run(['/opt/homebrew/bin/nvim','--server',str(rpc),'--remote-send',':qa!<CR>'],env=env,cwd=stage,timeout=3,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
  except Exception: process.terminate()
  try: process.wait(timeout=3)
  except subprocess.TimeoutExpired: process.kill();process.wait(timeout=3)
