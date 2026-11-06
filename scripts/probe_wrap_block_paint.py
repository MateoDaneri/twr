#!/usr/bin/env python3
"""Record synthetic Neovim VisualBlock screen attributes with wrap."""
import argparse,importlib.util,json,select,socket,subprocess,tempfile,threading,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('base',Path(__file__).resolve().parent/'probe_wrap_wheel_wide.py');base=importlib.util.module_from_spec(spec);spec.loader.exec_module(base)

def run(wheel=False, eol=False, intermediate=False, lazy_postkeys=False):
 out=[];nvim='/opt/homebrew/bin/nvim'
 for width,height in ((13,8),(27,10)):
  stage=Path(tempfile.mkdtemp(prefix='twr-block-paint-',dir='/private/tmp'));stage.chmod(0o700)
  env={'HOME':str(stage),'PATH':'/usr/bin:/bin','TMPDIR':str(stage),'XDG_CONFIG_HOME':str(stage/'config'),'XDG_STATE_HOME':str(stage/'state'),'XDG_DATA_HOME':str(stage/'data'),'XDG_CACHE_HOME':str(stage/'cache'),'NVIM_LOG_FILE':str(stage/'nvim.log')}
  rpc=stage/'rpc.sock';proc=subprocess.Popen([nvim,'--headless','-u','NONE','-i','NONE','--noplugin','-n','--listen',str(rpc)],env=env,cwd=stage,stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=(stage/'stderr.log').open('wb'))
  ui=None;worker=None;stop=threading.Event()
  try:
   deadline=time.monotonic()+8
   while not rpc.exists():
    if time.monotonic()>deadline or proc.poll() is not None:raise RuntimeError('Neovim failed: '+str(stage))
    time.sleep(.01)
   ui=socket.socket(socket.AF_UNIX);ui.connect(str(rpc));ui.sendall(base.pack([0,1,'nvim_ui_attach',[width,height,{'rgb':True,'ext_linegrid':True}]]))
   def drain():
    while not stop.is_set():
     try:
      ready,_,_=select.select([ui],[],[],.1)
      if ready and not ui.recv(65536):break
     except OSError:break
   worker=threading.Thread(target=drain,daemon=True);worker.start()
   def remote(expr):return subprocess.check_output([nvim,'--server',str(rpc),'--remote-expr',expr],env=env,cwd=stage,stderr=subprocess.PIPE,timeout=5).decode()
   remote("execute('set wrap linebreak smoothscroll nonumber norelativenumber signcolumn=no tabstop=8 scrolljump=1 scrolloff=4 mouse=a virtualedit=block')")
   if lazy_postkeys:
    remote("luaeval('vim.keymap.set(\"x\",\"j\",function() return vim.v.count==0 and \"gj\" or \"j\" end,{expr=true})')")
    remote("luaeval('vim.keymap.set(\"x\",\"k\",function() return vim.v.count==0 and \"gk\" or \"k\" end,{expr=true})')")
   rows=int(remote('winheight(0)'))
   gridexpr="json_encode(luaeval('(function() local o={} for r=1,vim.fn.winheight(0) do local a={} for c=1,vim.o.columns do a[#a+1]={vim.fn.screenstring(r,c),vim.fn.screenattr(r,c)} end o[#o+1]=a end return o end)()'))"
   scenarios=[(['abcde','A','xyz'],'ll<C-v>j'),(['alpha beta gamma delta epsilon','xy','z'],'4l<C-v>j5l'),(['a\\tb abcdefghijklmnop','x\\ty','last'],'2l<C-v>j4l'),(['é界 é界 é界 é界 é界','short','tail'],'2l<C-v>j4l')]
   if eol:
    scenarios=[(lines, 'l<C-v>j$' if keys=='ll<C-v>j' else '4l<C-v>j$' if keys=='4l<C-v>j5l' else '2l<C-v>j$') for lines,keys in scenarios]
   for lines,keys in scenarios:
    lines=[line.replace('\\t','\t') for line in lines]
    remote("nvim_input('<Esc>')");remote("luaeval('vim.api.nvim_buf_set_lines(0,0,-1,false,_A)',"+json.dumps(lines)+")");remote("execute('normal! gg0')")
    remote('nvim_input('+json.dumps(keys)+')');mode=remote('mode()')
    if not mode.startswith('\x16'):raise RuntimeError('Visual block did not remain active: '+repr(mode))
    remote("screenpos(0,line('.'),col('.'))")
    if wheel:
     before_view=json.loads(remote('json_encode(winsaveview())'));before_cursor=json.loads(remote('json_encode(getcurpos())'));wheels=['down','down','up']
     intermediate_events=[]
     for direction in wheels:
      remote("luaeval('vim.api.nvim_input_mouse(\"wheel\",_A,\"\",0,2,2)',"+json.dumps(direction)+")")
      remote("screenpos(0,line('.'),col('.'))")
      if intermediate:intermediate_events.append({'cursor':json.loads(remote('json_encode(getcurpos())')), 'view':json.loads(remote('json_encode(winsaveview())'))})
    if lazy_postkeys:
     post_moves=[]
     for key in ('k','k','j','j'):
      remote('nvim_input('+json.dumps(key)+')');remote("screenpos(0,line('.'),col('.'))")
      post_moves.append({'key':key,'cursor':json.loads(remote('json_encode(getcurpos())')),'view':json.loads(remote('json_encode(winsaveview())'))})
    view=json.loads(remote('json_encode(winsaveview())'));cursor=json.loads(remote('json_encode(getcurpos())'));anchor=json.loads(remote("json_encode(getpos('v'))")); selected=json.loads(remote(gridexpr))
    remote("nvim_input('<Esc>')");remote("luaeval('vim.fn.winrestview(_A)',"+json.dumps(view)+")");remote("screenpos(0,line('.'),col('.'))");plain=json.loads(remote(gridexpr))
    if [[c[0] for c in row] for row in selected] != [[c[0] for c in row] for row in plain]: raise RuntimeError('Screen changed after leaving Visual; comparison invalid')
    marks=[[1 if selected[r][c][1]!=plain[r][c][1] else 0 for c in range(width)] for r in range(rows)]
    case={'width':width,'height':height,'rows':rows,'lines':lines,'keys':keys,'view':view,'cursor':cursor,'anchor':anchor,'screen':[''.join(c[0] if c[0] else ' ' for c in row) for row in selected],'selected':marks}
    if wheel:case.update({'before_view':before_view,'before_cursor':before_cursor,'wheels':wheels})
    if intermediate:case['intermediate']=intermediate_events
    if lazy_postkeys:case['post_moves']=post_moves
    out.append(case)
  finally:
   try:subprocess.run([nvim,'--server',str(rpc),'--remote-send','<Esc>:qa!<CR>'],env=env,cwd=stage,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=3,check=False)
   except (OSError,subprocess.TimeoutExpired):pass
   try:proc.wait(timeout=3)
   except subprocess.TimeoutExpired:proc.kill();proc.wait()
   stop.set()
   if ui:ui.close()
   if worker:worker.join(timeout=1)
 version=subprocess.check_output([nvim,'--version'],env={'PATH':'/usr/bin:/bin'},timeout=5).decode().splitlines()[0]
 return {'version':version,'cases':out}

if __name__=='__main__':
 parser=argparse.ArgumentParser();parser.add_argument('--record',action='store_true');parser.add_argument('--wheel',action='store_true');parser.add_argument('--eol',action='store_true');parser.add_argument('--intermediate',action='store_true');parser.add_argument('--lazy-postkeys',action='store_true');args=parser.parse_args()
 if args.intermediate and not args.wheel:parser.error('--intermediate requires --wheel')
 if args.lazy_postkeys and not (args.wheel and args.intermediate):parser.error('--lazy-postkeys requires --wheel --intermediate')
 result=run(args.wheel,args.eol,args.intermediate,args.lazy_postkeys);name=(('nvim-wrap-block-eol-lazy-postkeys.json' if args.eol else 'nvim-wrap-block-lazy-postkeys.json') if args.lazy_postkeys else ('nvim-wrap-block-eol-wheel-intermediate.json' if args.intermediate else 'nvim-wrap-block-eol-wheel-paint.json' if args.wheel and args.eol else 'nvim-wrap-block-eol-paint.json' if args.eol else 'nvim-wrap-block-wheel-paint.json' if args.wheel else 'nvim-wrap-block-paint.json'));path=Path(__file__).resolve().parents[1]/'Resources/Fixtures'/name
 if args.record and path.is_symlink():raise RuntimeError('Refusing a symlink fixture')
 if args.record:path.write_text(json.dumps(result,ensure_ascii=False,separators=(',',':'))+'\n')
 elif json.loads(path.read_text()) != result:raise RuntimeError('Screen reference differs from saved fixture')
 print('synthetic block paint cases',len(result['cases']),'recorded',args.record)
