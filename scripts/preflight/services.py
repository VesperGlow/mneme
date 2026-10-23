#!/usr/bin/env python3
"""Prepare/start ONLY owned disposable Neo4j and the repository's pinned CPU models.

Usage: python3 scripts/preflight/services.py --runtime /tmp/mneme-preflight-runtime --download --start
Existing occupied ports are refused. Ctrl-C stops only processes started by this invocation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import time
import urllib.request

ROOT=Path(__file__).resolve().parents[2]
ARTIFACTS={
 'neo4j':('https://dist.neo4j.org/neo4j-community-5.26.32-unix.tar.gz','d279ae6a9b38034280681a558b0cd222dce89f87cc3d9867f1f7bd28ef5ebc30'),
 'jre':('https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.13%2B9/OpenJDK21U-jre_x64_linux_hotspot_21.0.13_9.tar.gz','a535a1df9c6284d9618872984e026ae0352d1f7e9e0605425c0e6114f47351dd')}


def command(args,**kwargs):
    subprocess.run(args,check=True,cwd=ROOT,**kwargs)


def unused(port):
    with socket.socket() as sock:
        try: sock.bind(('127.0.0.1',port))
        except OSError: raise SystemExit(f'Port {port} is occupied; no existing service will be stopped') from None


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--runtime',type=Path,required=True)
    parser.add_argument('--download',action='store_true')
    parser.add_argument('--start',action='store_true')
    parser.add_argument('--cpu-threads',type=int,default=3)
    parser.add_argument('--embedding-dtype',choices=('float32','bfloat16'),default='float32')
    args=parser.parse_args(); runtime=args.runtime.resolve()
    if runtime.exists() and not (runtime/'.mneme-preflight-runtime-owned').is_file():
        parser.error('existing directory has no ownership marker; choose a new temporary directory')
    runtime.mkdir(parents=True,exist_ok=True); runtime.chmod(0o700)
    (runtime/'.mneme-preflight-runtime-owned').write_text('mneme-local-preflight-v1\n')
    (runtime/'downloads').mkdir(exist_ok=True)
    if args.download:
        for name,(url,sha) in ARTIFACTS.items():
            path=runtime/'downloads'/f'{name}.tar.gz'
            if not path.exists():
                with urllib.request.urlopen(url,timeout=180) as src,path.with_suffix('.partial').open('wb') as dst: shutil.copyfileobj(src,dst)
                path.with_suffix('.partial').replace(path)
            with path.open('rb') as f:
                if hashlib.file_digest(f,'sha256').hexdigest()!=sha: raise SystemExit(f'{name} checksum mismatch')
        graph=runtime/'neo4j-community-5.26.32'
        if not graph.is_dir():
            with tarfile.open(runtime/'downloads/neo4j.tar.gz') as archive: archive.extractall(runtime,filter='data')
        if not (runtime/'jre').is_dir():
            stage=runtime/'java-extract'; stage.mkdir(exist_ok=True)
            with tarfile.open(runtime/'downloads/jre.tar.gz') as archive: archive.extractall(stage,filter='data')
            next(stage.iterdir()).rename(runtime/'jre')
        if not (runtime/'venv/bin/python').is_file(): command([sys.executable,'-m','venv',str(runtime/'venv')])
        pip=str(runtime/'venv/bin/pip')
        command([pip,'install','--disable-pip-version-check','torch==2.8.0','--index-url','https://download.pytorch.org/whl/cpu'])
        command([pip,'install','--disable-pip-version-check','-r',str(ROOT/'services/models/requirements.txt'),'requests','psutil','tiktoken==0.12.0'])
        env={**os.environ,'HF_HOME':str(runtime/'hf'),'PYTHONPATH':str(ROOT/'services/models')}
        command([str(runtime/'venv/bin/python'),'-c','from artifacts import prepare_cache; prepare_cache()'],env=env)
    if not args.start: return
    unused(18474); unused(19090)
    graph=runtime/'neo4j-community-5.26.32'
    if not graph.is_dir() or not (runtime/'venv/bin/python').exists(): parser.error('missing runtime; run --download first')
    config='''server.default_listen_address=127.0.0.1
server.http.listen_address=127.0.0.1:18474
server.http.advertised_address=127.0.0.1:18474
server.bolt.enabled=false
server.https.enabled=false
server.memory.heap.initial_size=256m
server.memory.heap.max_size=512m
server.memory.pagecache.size=64m
db.tx_log.preallocate=false
db.tx_log.rotation.size=16M
db.tx_log.rotation.retention_policy=2 files
dbms.usage_report.enabled=false
'''
    (graph/'conf/neo4j.conf').write_text(config)
    env={k:v for k,v in os.environ.items() if not k.startswith('MNEME_')}
    env.update(JAVA_HOME=str(runtime/'jre'))
    if not (graph/'data/databases').exists(): command([str(graph/'bin/neo4j-admin'),'dbms','set-initial-password','mneme-preflight-only-20261023'],env=env)
    children=[]; logs=[]
    try:
        log=(runtime/'neo4j-console.log').open('a'); logs.append(log)
        neo=subprocess.Popen([str(graph/'bin/neo4j'),'console'],env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True); children.append(neo)
        modelenv={**env,'HF_HOME':str(runtime/'hf'),'HF_HUB_OFFLINE':'1','MNEME_CPU_THREADS':str(args.cpu_threads),'MNEME_CPU_DTYPE':args.embedding_dtype,'MNEME_CPU_RERANK_DTYPE':'float32','PYTHONPATH':str(ROOT/'services/models')}
        log=(runtime/'models.log').open('a'); logs.append(log)
        models=subprocess.Popen([str(runtime/'venv/bin/python'),'-m','uvicorn','app:app','--host','127.0.0.1','--port','19090','--workers','1','--no-access-log'],env=modelenv,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,start_new_session=True); children.append(models)
        deadline=time.monotonic()+180
        while time.monotonic()<deadline:
            if any(p.poll() is not None for p in children): raise SystemExit('owned service exited; inspect runtime logs')
            try:
                urllib.request.urlopen('http://127.0.0.1:18474/',timeout=1).close()
                urllib.request.urlopen('http://127.0.0.1:19090/health',timeout=1).close()
                break
            except OSError: time.sleep(1)
        else: raise SystemExit('owned service startup timeout')
        pids=[]
        for entry in Path('/proc').iterdir():
            if not entry.name.isdigit(): continue
            try: cmd=(entry/'cmdline').read_bytes().decode().split('\0')
            except (OSError,UnicodeDecodeError): continue
            if 'org.neo4j.server.CommunityEntryPoint' in cmd and any(str(graph) in arg for arg in cmd):
                pids.append({'pid':int(entry.name),'command_verified':True})
        owner={'url':'http://127.0.0.1:18474','root':str(graph),'run_root':str(runtime),'processes':pids}
        (runtime/'graph-owner.json').write_text(json.dumps(owner,indent=2)+'\n')
        print('Owned Neo4j :18474 and CPU models :19090 ready; Ctrl-C stops only these test processes.',flush=True)
        while True:
            if any(p.poll() is not None for p in children): raise SystemExit('owned service stopped unexpectedly')
            time.sleep(1)
    except KeyboardInterrupt: pass
    finally:
        for process in reversed(children):
            if process.poll() is None:
                os.killpg(process.pid,signal.SIGTERM)
                try: process.wait(timeout=15)
                except subprocess.TimeoutExpired: os.killpg(process.pid,signal.SIGKILL); process.wait()
        for log in logs: log.close()


if __name__=='__main__': main()
