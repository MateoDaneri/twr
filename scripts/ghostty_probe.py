#!/usr/bin/env python3
"""Private, pinned libghostty-vt evaluation. Does not change the app's engine."""
import argparse
import base64
import concurrent.futures
import datetime
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request

COMMIT='8ef0ec1238220a5a714be452ed2844f0691fcf83'
ZIG_VERSION='0.17.0'
ZIG_URL='https://ziglang.org/download/0.17.0/zig-aarch64-macos-0.17.0.tar.xz'
ZIG_SHA='b607e9b9234790a008116ae5bdb71c6243b84b9fb42a53a9e70fde41c06c536a'
ZIG_SIZE=53985220
MIN_FREE=2*1024**3


def private(stage):
    stage=stage.resolve(strict=True)
    info=stage.lstat()
    if stage.parent != Path('/private/tmp') or not stage.name.startswith('tw-ghostty-probe-') or not stat.S_ISDIR(info.st_mode) or info.st_uid!=os.getuid() or info.st_mode&0o077:
        raise RuntimeError('Expected a private owned tw-ghostty-probe-* directory')
    return stage


def sha256(path):
    h=hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda:stream.read(1024**2),b''):h.update(chunk)
    return h.hexdigest()


def child(stage,path):
    current=stage
    for part in path.relative_to(stage).parts:
        current/=part
        if current.is_symlink():raise RuntimeError('Symlink in prepared input')
    return path


def get(url, target, limit, expected=None):
    started=time.monotonic();size=0
    request=urllib.request.Request(url,headers={'User-Agent':'Terminal-Workspace-pinned-evaluation'})
    with urllib.request.urlopen(request,timeout=20) as response, target.open('xb') as stream:
        while True:
            if time.monotonic()-started>120:raise RuntimeError('Download exceeded120s')
            chunk=response.read(65536)
            if not chunk:break
            size+=len(chunk)
            if size>limit:raise RuntimeError('Download exceeds byte limit')
            if shutil.disk_usage(target.parent).free<MIN_FREE:raise RuntimeError('Download stopped at2GiB free guard')
            stream.write(chunk)
    digest=sha256(target)
    if expected and digest!=expected:raise RuntimeError('Checksum mismatch: '+target.name)
    return {'url':url,'bytes':size,'sha256':digest}


def extract(archive, destination, prefix, maximum, expected_blobs=None):
    destination.mkdir(mode=0o700)
    total=0;count=0;verified={}
    with tarfile.open(archive) as tar:
        for index,member in enumerate(tar):
            if index>=50000:raise RuntimeError('Archive member budget exceeded')
            parts=PurePosixPath(member.name).parts
            if not parts or parts[0]!=prefix or PurePosixPath(member.name).is_absolute() or '..' in parts:
                raise RuntimeError('Unsafe archive path')
            if len(parts)==1:
                if member.isdir():continue
                raise RuntimeError('Invalid archive root')
            rel='/'.join(parts[1:]);target=destination.joinpath(*parts[1:])
            if member.isdir():target.mkdir(mode=0o700,parents=True,exist_ok=True);continue
            if not member.isfile():raise RuntimeError('Nonregular archive member: '+rel)
            total+=member.size;count+=1
            if total>maximum or count>50000 or member.size>256*1024**2:raise RuntimeError('Extraction budget exceeded')
            if shutil.disk_usage(destination).free<MIN_FREE:raise RuntimeError('Extraction stopped at2GiB free guard')
            if expected_blobs is not None and rel not in expected_blobs:raise RuntimeError('Unexpected source file: '+rel)
            target.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
            h=hashlib.sha1();h.update(('blob '+str(member.size)+'\0').encode())
            source=tar.extractfile(member)
            with target.open('xb') as output:
                for chunk in iter(lambda:source.read(65536),b''):h.update(chunk);output.write(chunk)
            target.chmod(0o700 if member.mode&0o111 else 0o600)
            if expected_blobs is not None:
                if h.hexdigest()!=expected_blobs[rel]:raise RuntimeError('Git source blob mismatch: '+rel)
                verified[rel]=h.hexdigest()
    if expected_blobs is not None and set(verified)!=set(expected_blobs):raise RuntimeError('Source archive missing Git blobs')
    return {'files':count,'bytes':total,'blobs':verified}


def environment(stage):
    home=stage/'home';home.mkdir(mode=0o700,exist_ok=True)
    env={'HOME':str(home),'PATH':'/usr/bin:/bin:/usr/sbin:/sbin','TMPDIR':str(stage),
         'XDG_CONFIG_HOME':str(home/'config'),'XDG_DATA_HOME':str(home/'data'),
         'XDG_STATE_HOME':str(home/'state'),'XDG_CACHE_HOME':str(home/'cache'),
         'ZIG_GLOBAL_CACHE_DIR':str(stage/'zig-global-cache'),'ZIG_LOCAL_CACHE_DIR':str(stage/'zig-local-cache'),
         'GIT_CONFIG_NOSYSTEM':'1','GIT_CONFIG_GLOBAL':'/dev/null','GIT_TERMINAL_PROMPT':'0'}
    return env


def prepare():
    if platform.system()!='Darwin' or platform.machine()!='arm64':raise RuntimeError('This probe is pinned to macOS arm64')
    if shutil.disk_usage('/private/tmp').free<3*1024**3:raise RuntimeError('Prepare requires at least3GiB free')
    stage=Path(tempfile.mkdtemp(prefix='tw-ghostty-probe-',dir='/private/tmp'));print(stage,flush=True)
    receipt={'commit':COMMIT,'zig_version':ZIG_VERSION,'created':datetime.datetime.now(datetime.timezone.utc).isoformat()}
    jobs=[('zig',ZIG_URL,stage/'zig.tar.xz',ZIG_SIZE,ZIG_SHA),
          ('source','https://codeload.github.com/ghostty-org/ghostty/tar.gz/'+COMMIT,stage/'source.tar.gz',100*1024**2,None),
          ('tree','https://api.github.com/repos/ghostty-org/ghostty/git/trees/'+COMMIT+'?recursive=1',stage/'tree.json',8*1024**2,None)]
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        futures={pool.submit(get,url,target,limit,expected):name for name,url,target,limit,expected in jobs}
        for future in concurrent.futures.as_completed(futures):
            name=futures[future];receipt[name]=future.result();print('Downloaded',name,receipt[name]['bytes'],flush=True)
    if receipt['zig']['bytes']!=ZIG_SIZE:raise RuntimeError('Unexpected Zig archive size')
    tree=json.loads((stage/'tree.json').read_text())
    if tree.get('truncated'):raise RuntimeError('Truncated Git tree')
    if any(i.get('mode')=='120000' for i in tree['tree']):raise RuntimeError('Source tree contains symlinks')
    blobs={i['path']:i['sha'] for i in tree['tree'] if i['type']=='blob'}
    receipt['source_files']=extract(stage/'source.tar.gz',stage/'source','ghostty-'+COMMIT,128*1024**2,blobs)
    receipt['zig_files']=extract(stage/'zig.tar.xz',stage/'toolchain','zig-aarch64-macos-'+ZIG_VERSION,768*1024**2)
    receipt['zig_binary_sha256']=sha256(stage/'toolchain/zig')
    receipt['zig_binary_bytes']=(stage/'toolchain/zig').stat().st_size
    version=subprocess.run([str(stage/'toolchain/zig'),'version'],env=environment(stage),cwd=stage,capture_output=True,timeout=5,check=True).stdout.decode().strip()
    if version!=ZIG_VERSION:raise RuntimeError('Zig version mismatch')
    (stage/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
    print('Verified Zig',version,'and',len(blobs),'source blobs',flush=True)
    print('Prepared:',stage,flush=True)


def checked(stage):
    stage=private(stage);receipt=json.loads((stage/'receipt.json').read_text())
    zig=child(stage,stage/'toolchain/zig')
    if receipt['commit']!=COMMIT or receipt['zig_version']!=ZIG_VERSION or zig.stat().st_size!=receipt['zig_binary_bytes'] or sha256(zig)!=receipt['zig_binary_sha256']:
        raise RuntimeError('Prepared toolchain identity changed')
    for rel,expected in receipt['source_files']['blobs'].items():
        path=child(stage,stage/'source'/rel)
        if not path.is_file() or path.stat().st_size>256*1024**2:raise RuntimeError('Source file replaced: '+rel)
        data=path.read_bytes()
        if hashlib.sha1(('blob '+str(len(data))+'\0').encode()+data).hexdigest()!=expected:raise RuntimeError('Source file changed: '+rel)
    return stage


def run_owned(stage,command,log,timeout=300):
    started=time.monotonic();failure=None;cleanup_errors=[]
    record=log.with_suffix('.json')
    result={'command':command,'log':str(log),'state':'starting'}
    def save():
        record.write_text(json.dumps(result,indent=2)+'\n')
    save()
    with log.open('xb') as output:
        process=subprocess.Popen(command,env=environment(stage),cwd=stage/'source',stdout=output,stderr=subprocess.STDOUT,start_new_session=True)
        result.update(pid=process.pid,process_group=process.pid,state='running')
        save()
        def signal_group(value):
            try:os.killpg(process.pid,value)
            except ProcessLookupError:pass
            except PermissionError as error:
                cleanup_errors.append(str(error))
                # The direct child is ours even when group signalling is blocked.
                if process.poll() is None:
                    try:process.send_signal(value)
                    except (ProcessLookupError,PermissionError) as child_error:
                        cleanup_errors.append(str(child_error))
        try:
            while process.poll() is None:
                if time.monotonic()-started>timeout:failure='time budget';break
                if shutil.disk_usage(stage).free<MIN_FREE:failure='2GiB free guard';break
                time.sleep(0.25)
            if failure:
                signal_group(signal.SIGTERM)
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:signal_group(signal.SIGKILL)
        finally:
            signal_group(signal.SIGKILL)
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:cleanup_errors.append('Direct child did not exit within cleanup budget')
            result.update(returncode=process.poll(),seconds=round(time.monotonic()-started,3),
                          stopped=failure,cleanup_errors=cleanup_errors,
                          state='cleanup_unverified' if cleanup_errors else 'finished')
            save()
    print(json.dumps(result),flush=True)
    return result


def supervision(stage):
    """A single, self-expiring child checks group signalling before any build."""
    stage=private(stage)
    result={'state':'starting','group_term_error':None,'child_term_error':None}
    record=stage/('supervision-'+str(time.time_ns())+'.json')
    process=subprocess.Popen([sys.executable,'-I','-S','-c','import time; time.sleep(3)'],
                             cwd=stage,env=environment(stage),start_new_session=True,
                             stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    result.update(pid=process.pid,process_group=process.pid)
    try:
        try:os.killpg(process.pid,signal.SIGTERM)
        except OSError as error:
            result['group_term_error']=str(error)
            try:process.terminate()
            except OSError as child_error:result['child_term_error']=str(child_error)
        try:process.wait(timeout=4)
        except subprocess.TimeoutExpired:
            result['child_term_error']='Self-expiring child exceeded4s'
        result['returncode']=process.poll()
        result['state']='passed' if result['group_term_error'] is None and result['returncode']==-signal.SIGTERM else 'failed'
    finally:
        record.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({'supervision':result,'receipt':str(record)}),flush=True)
    if result['state']!='passed':raise RuntimeError('Own process group supervision unavailable; build not started')
    return result


def build(stage):
    stage=checked(stage)
    if shutil.disk_usage(stage).free<MIN_FREE:raise RuntimeError('Build requires2GiB free')
    supervision(stage)
    stamp=str(time.time_ns());log=stage/('build-'+stamp+'.log')
    command=[str(stage/'toolchain/zig'),'build','-Demit-lib-vt=true','-Doptimize=ReleaseSafe','-Dsimd=false','-Demit-xcframework=false','-j2','--prefix',str(stage/'install')]
    result=run_owned(stage,command,log)
    if result['returncode']==0 and not result['stopped'] and not result['cleanup_errors']:
        libs=[stage/'install/lib/libghostty-vt.a',stage/'install/lib/libghostty-vt.dylib']
        result['artifacts']=[{'path':str(p),'bytes':p.stat().st_size,'sha256':sha256(p)} for p in libs if p.is_file()]
    (stage/('build-'+stamp+'.json')).write_text(json.dumps(result,indent=2)+'\n')
    if result['returncode'] or result['stopped'] or result['cleanup_errors']:raise RuntimeError('Native evaluation build did not pass; log and process receipt retained')


def probe(stage):
    stage=checked(stage)
    receipts=sorted(stage.glob('build-*.json'))
    if not receipts:raise RuntimeError('No completed build receipt')
    receipt=json.loads(receipts[-1].read_text())
    if receipt.get('state')!='finished' or receipt.get('returncode')!=0 or receipt.get('stopped') or receipt.get('cleanup_errors'):
        raise RuntimeError('Latest build is not verified')
    library=child(stage,stage/'install/lib/libghostty-vt.a')
    matching=[a for a in receipt.get('artifacts',[]) if a['path']==str(library)]
    if len(matching)!=1 or sha256(library)!=matching[0]['sha256']:raise RuntimeError('Library does not match build receipt')
    compiler=Path('/usr/bin/clang')
    if not compiler.is_file():raise RuntimeError('Apple clang is unavailable')
    version=subprocess.run([str(compiler),'--version'],env=environment(stage),capture_output=True,timeout=5,check=True).stdout.decode()
    output=Path(tempfile.mkdtemp(prefix='probe-',dir=stage));print(output,flush=True)
    fixture=Path(__file__).resolve().parent/'fixtures/terminal_core_probe.c'
    command=[str(compiler),'-std=c11','-Wall','-Wextra','-Werror','-I',str(stage/'source/include'),str(fixture),str(library),'-o',str(output/'core-probe')]
    compile_result=run_owned(stage,command,output/'compile.log',timeout=30)
    if compile_result['returncode'] or compile_result['stopped'] or compile_result['cleanup_errors']:
        raise RuntimeError('C probe did not compile; log retained')
    sentinel=output/'own-red-pixel.rgba';sentinel.write_bytes(bytes([255,0,0,255]))
    sentinel_arg=base64.b64encode(str(sentinel).encode()).decode()
    shm_arg=base64.b64encode(('/tw-ghostty-'+output.name+'-absent').encode()).decode()
    run_result=run_owned(stage,[str(output/'core-probe'),sentinel_arg,shm_arg],output/'run.log',timeout=10)
    checks=[json.loads(line) for line in (output/'run.log').read_text().splitlines() if line.startswith('{')]
    checks.append({'check':'owned_file_fixture_unchanged','passed':sentinel.exists() and sentinel.read_bytes()==bytes([255,0,0,255])})
    passed=run_result['returncode']==0 and not run_result['stopped'] and not run_result['cleanup_errors'] and len(checks)==27 and all(c.get('passed') is True for c in checks)
    result={'status':'passed' if passed else 'failed','source_commit':COMMIT,'build_receipt':str(receipts[-1]),'library_sha256':sha256(library),'fixture_sha256':sha256(fixture),'compiler':version,'binary_sha256':sha256(output/'core-probe'),'compile':compile_result,'run':run_result,'checks':checks,'scope':'Headless synthetic VT; no GUI, real PTY, PNG decoding, host clipboard or network proof.'}
    (output/'results.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({'status':result['status'],'checks':len(checks),'results':str(output/'results.json')}),flush=True)
    if not passed:raise RuntimeError('Core conformance failed; evidence retained')


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('action',choices=['prepare','build','supervision','probe']);parser.add_argument('--stage',type=Path);args=parser.parse_args()
    if args.action=='prepare':prepare()
    elif args.stage is None:parser.error('--stage is required for this action')
    elif args.action=='supervision':supervision(args.stage)
    elif args.action=='probe':probe(args.stage)
    else:build(args.stage)
