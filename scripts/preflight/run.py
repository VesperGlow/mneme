#!/usr/bin/env python3
"""Owned, loopback-only preflight. Stress is always mocked; live LLM uses a bounded relay.

Never loads .env. All SQLite access requires an owned run directory. Neo4j writes
require a run-specific marker in a dedicated database; existing services are never
stopped by this runner. Live provider credentials are read from a 0600 file only.
"""
import argparse
import asyncio
import concurrent.futures
import hashlib
import json
import math
import os
from pathlib import Path
import random
import re
import signal
import socket
import sqlite3
import subprocess
import sys
import threading
import time
import uuid
from collections import Counter

import psutil
import requests
import tiktoken
import uvicorn
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, Response

ROOT = Path(__file__).resolve().parents[2]
TOKEN = 'mneme-preflight-only-01234567890123456789'
GRAPH_PASSWORD = 'mneme-preflight-only-20261023'
ENC = tiktoken.get_encoding('o200k_base')


def percentiles(values):
    values = sorted(values)
    return {f'p{p}_ms': values[max(0, math.ceil(len(values)*p/100)-1)] if values else None for p in (50,95,99)}


def terms(text):
    text = text.lower()
    words = set(re.findall(r'[a-z0-9_:.+-]+', text))
    for run in re.findall(r'[\u3400-\u9fff]+', text):
        words.update(run[i:i+2] for i in range(len(run)-1))
    return words


def lucene(text):
    tokens=set()
    chinese=[]; word=[]
    def flush():
        if len(chinese)==1: tokens.add(chinese[0])
        tokens.update(''.join(chinese[i:i+2]) for i in range(len(chinese)-1))
        chinese.clear()
        if word: tokens.add(''.join(word)); word.clear()
    for char in text[:512]:
        if '\u3400'<=char<='\u9fff':
            if word: tokens.add(''.join(word)); word.clear()
            chinese.append(char)
        elif char.isalnum():
            if chinese: flush()
            word.append(char.lower())
        else: flush()
    flush()
    return ' OR '.join('"'+term+'"' for term in sorted(tokens)[:64])


def vector(text):
    result = [0.0]*1024
    for word in terms(text):
        digest = hashlib.sha256(word.encode()).digest()
        result[int.from_bytes(digest[:2], 'little')%1024] += 1
    norm = sum(v*v for v in result)**0.5 or 1
    return [v/norm for v in result]


def metric(ranked, gold, k=30):
    gold = set(gold)
    return {'recall_at_30':len(set(ranked[:k]) & gold)/len(gold) if gold else None,
            'precision_at_5':len(set(ranked[:5]) & gold)/5,
            'mrr':next((1/(i+1) for i,x in enumerate(ranked) if x in gold),0) if gold else None,
            'returned_precision':len(set(ranked[:5]) & gold)/min(5,len(ranked)) if ranked else None,
            'negative_contamination':bool(ranked) if not gold else None}


def aggregate(rows):
    result = {}
    for name in ('recall_at_30','precision_at_5','mrr','returned_precision','negative_contamination'):
        values = [r[name] for r in rows if r.get(name) is not None]
        result[name] = sum(values)/len(values) if values else None
    return result


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1',0))
        return sock.getsockname()[1]


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix+'.partial')
    temporary.write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n')
    temporary.replace(path)


class TcpProxy:
    """Fault-only TCP bridge: resets real keep-alive sockets without touching Neo4j."""
    def __init__(self, upstream):
        self.upstream=int(upstream.rsplit(':',1)[1]); self.port=free_port()
        self.loop=asyncio.new_event_loop(); self.writers=set(); self.tasks=set(); self.listener=None

    async def connect(self, reader, writer):
        task=asyncio.current_task(); self.tasks.add(task); other=None; copies=[]
        try:
            remote,other=await asyncio.open_connection('127.0.0.1',self.upstream)
            self.writers.update([writer,other])
            async def copy(src,dst):
                while data:=await src.read(65536): dst.write(data); await dst.drain()
            a=asyncio.create_task(copy(reader,other)); b=asyncio.create_task(copy(remote,writer))
            copies=[a,b]
            done,pending=await asyncio.wait([a,b],return_when=asyncio.FIRST_COMPLETED)
            for pending_task in pending: pending_task.cancel()
            await asyncio.gather(a,b,return_exceptions=True)
        except (OSError,asyncio.CancelledError): pass
        finally:
            for copy_task in copies: copy_task.cancel()
            if copies: await asyncio.gather(*copies,return_exceptions=True)
            for stream in (writer,other):
                if stream: self.writers.discard(stream); stream.close()
            self.tasks.discard(task)

    async def resume_async(self):
        self.listener=await asyncio.start_server(self.connect,'127.0.0.1',self.port)

    def start(self):
        def run():
            asyncio.set_event_loop(self.loop); self.loop.run_forever()
        self.thread=threading.Thread(target=run,daemon=True); self.thread.start()
        self.resume(); return self

    def resume(self): asyncio.run_coroutine_threadsafe(self.resume_async(),self.loop).result(5)

    def suspend(self):
        async def close():
            listener=self.listener; self.listener=None
            if listener: listener.close()
            # Python 3.13 wait_closed also waits for client transports. Reset
            # them before waiting, otherwise fault injection itself deadlocks.
            for stream in list(self.writers): stream.transport.abort()
            tasks=list(self.tasks)
            for task in tasks: task.cancel()
            if tasks: await asyncio.gather(*tasks,return_exceptions=True)
            if listener: await listener.wait_closed()
        asyncio.run_coroutine_threadsafe(close(),self.loop).result(5)

    def stop(self):
        self.suspend(); self.loop.call_soon_threadsafe(self.loop.stop); self.thread.join(5); self.loop.close()


class Fixture:
    def __init__(self, graph_url, real_models, provider_file, out):
        self.graph_url, self.real_models, self.out = graph_url, real_models, out
        if provider_file and provider_file.stat().st_mode & 0o077:
            raise ValueError('credential file must be private (0600)')
        self.provider = json.loads(provider_file.read_text()) if provider_file else None
        if self.provider and (self.provider.get('base_url') != 'https://api.deepseek.com/v1' or self.provider.get('model') != 'deepseek-flash'):
            raise ValueError('provider destination/model is not the authorized DeepSeek endpoint')
        self.lock = threading.Lock()
        self.mode = 'mock'
        self.fault = None
        self.extractions, self.requests, self.live_responses = [], [], []
        self.live_calls, self.live_tokens, self.reserved_tokens = 0, 0, 0
        self.max_live_calls, self.max_live_tokens = 48, 500_000
        self.pause = threading.Event(); self.pause.set()
        self.app = app = FastAPI()

        @app.post('/v1/chat/completions')
        async def llm(request: Request):
            body = await request.json()
            if body.get('stream'):
                answer = '我听到了，继续说吧。'
                wire = 'data: '+json.dumps({'choices':[{'delta':{'content':answer},'finish_reason':'stop'}]},ensure_ascii=False)+'\n\ndata: [DONE]\n\n'
                return Response(wire,media_type='text/event-stream')
            data = json.loads(body['messages'][1]['content'])
            if 'max_tokens' in data: # independent context compaction
                messages = data.get('messages',[])
                summary = (data.get('previous_summary','')+'\n'+'\n'.join(m.get('content','')[:100] for m in messages if m['role']=='user'))[-1200:]
                return self.completion({'summary':summary})
            targets = data.get('targets',[data.get('target',{})])
            with self.lock:
                self.extractions.append({'at':time.time(),'source_ids':[t.get('id') for t in targets],
                                         'source_hashes':[hashlib.sha256(t.get('content','').encode()).hexdigest() for t in targets],
                                         'mode':self.mode,'fault':self.fault})
            if self.fault == 'invalid_llm': return self.completion('this is invalid JSON',raw=True)
            if self.fault == 'pause_llm': await asyncio.to_thread(self.pause.wait,30)
            if self.mode == 'live': return await asyncio.to_thread(self.live,body)
            memories = []
            for target in targets:
                text = target.get('content','')
                if re.fullmatch(r'[嗯哦好呀的谢谢收到晚安早安，。！\s]+',text): continue
                sentences = [x.strip() for x in re.split(r'[。！？\n]+',text) if x.strip()]
                # Retain meaningful head/tail markers while excluding synthetic padding.
                quotes = [x for x in sentences if not x.startswith('填充资料')]
                for quote in list(dict.fromkeys(quotes))[:8]:
                    item = {'key':'user.event.'+hashlib.sha256(quote.encode()).hexdigest()[:24],
                            'content':'用户表示：'+quote[:470], 'kind':'event','tier':'episodic','evidence_type':'user_quote',
                            'importance':0.01,'confidence':0.99,'entities':re.findall(r'[A-Za-z][A-Za-z0-9_.:-]+',quote)[:8],
                            'source_quote':quote,'valid_until':None,'sensitive':False,'equivalent_to':None,
                            'attribute_of':None,'relation':'coexist','supersedes':[]}
                    if 'targets' in data: item['source_id'] = target['id']
                    memories.append(item)
            result = {'memories':memories[:32]}
            if 'targets' in data: result['processed_source_ids'] = [t['id'] for t in targets]
            return self.completion(result)

        @app.post('/embed')
        async def embed(request: Request):
            body = await request.json()
            if self.fault == 'embedding_timeout': await asyncio.sleep(2.5)
            if self.fault == 'embedding_unavailable': return JSONResponse({},status_code=503)
            if self.mode in ('live','live_models'): return await asyncio.to_thread(self.forward_models,'embed',body)
            return {'embeddings':[vector(t) for t in body['texts']]}

        @app.post('/rerank')
        async def rerank(request: Request):
            body = await request.json()
            if self.fault == 'reranker_invalid': return {'scores':['not-a-number']}
            if self.mode in ('live','live_models','real_reranker'): return await asyncio.to_thread(self.forward_models,'rerank',body)
            query = terms(body['query'])
            return {'scores':[min(1.0,3*len(query & terms(d))/max(1,math.sqrt(len(query)*len(terms(d))))) for d in body['documents']]}

        @app.post('/db/neo4j/tx/commit')
        async def graph(request: Request):
            body = await request.json()
            if self.fault == 'graph_unavailable': return JSONResponse({},status_code=503)
            return await asyncio.to_thread(self.forward_graph,body)

    @staticmethod
    def completion(value,raw=False):
        return JSONResponse({'choices':[{'message':{'content':value if raw else json.dumps(value,ensure_ascii=False)},'finish_reason':'stop'}]})

    def forward_graph(self,body):
        r = requests.post(self.graph_url+'/db/neo4j/tx/commit',auth=('neo4j',GRAPH_PASSWORD),json=body,timeout=15)
        return JSONResponse(r.json(),status_code=r.status_code)

    def forward_models(self,method,body):
        r = requests.post(self.real_models+'/'+method,json=body,timeout=60)
        return JSONResponse(r.json(),status_code=r.status_code)

    def live(self,body):
        if not self.provider: return JSONResponse({'error':'live_provider_not_configured'},status_code=503)
        body = dict(body)
        body['thinking'] = {'type':'disabled'}
        body['model'] = 'deepseek-flash'; body.pop('response_format',None)
        reserve = len(ENC.encode(json.dumps(body['messages'],ensure_ascii=False)))+body.get('max_tokens',8000)
        with self.lock:
            if self.live_calls>=self.max_live_calls or self.live_tokens+self.reserved_tokens+reserve>self.max_live_tokens:
                return JSONResponse({'error':'preflight_budget_exhausted'},status_code=429)
            self.live_calls += 1; self.reserved_tokens += reserve
        start = time.monotonic()
        try:
            r = requests.post(self.provider['base_url']+'/chat/completions',headers={'Authorization':'Bearer '+self.provider['key']},json=body,timeout=100)
            value = r.json()
            with self.lock:
                self.reserved_tokens -= reserve
                self.live_tokens += value.get('usage',{}).get('total_tokens',reserve)
                self.live_responses.append({'http_status':r.status_code,'elapsed_ms':1000*(time.monotonic()-start),
                                           'usage':value.get('usage'),'response':value.get('choices'),
                                           'input':body['messages']})
                write_json(self.out/'live-provider-evidence.json',{'model':'deepseek-flash','requests':self.live_calls,'tokens':self.live_tokens,'rows':self.live_responses})
            return JSONResponse(value,status_code=r.status_code)
        except Exception as error:
            with self.lock:
                self.reserved_tokens -= reserve; self.live_tokens += reserve
            return JSONResponse({'error':type(error).__name__},status_code=502)

    def start(self):
        self.port = free_port()
        self.server = uvicorn.Server(uvicorn.Config(self.app,host='127.0.0.1',port=self.port,log_level='error',access_log=False))
        self.thread = threading.Thread(target=self.server.run,daemon=True); self.thread.start()
        deadline = time.monotonic()+10
        while not self.server.started:
            if time.monotonic()>deadline: raise RuntimeError('fixture startup timeout')
            time.sleep(.02)
        return self

    def stop(self):
        self.pause.set(); self.server.should_exit=True; self.thread.join(5)


class Server:
    def __init__(self, run, name, fixture, *, extract=2048, context=16000, model_timeout=1, retrieval_timeout=3, reconcile=False, direct_graph=False, graph_url=None, attempts=3):
        self.run,self.name,self.fixture=run,name,fixture
        self.root = run.out/name; self.root.mkdir(parents=True,exist_ok=True)
        (self.root/'.mneme-preflight-owned').write_text(run.id)
        self.db = self.root/'test.sqlite3'; self.port=free_port(); self.url=f'http://127.0.0.1:{self.port}'
        config = (ROOT/'config/default.toml').read_text()
        config=config.replace('listen = "127.0.0.1:8080"',f'listen = "127.0.0.1:{self.port}"').replace('database = "data/mneme.sqlite3"',f'database = "{self.db}"')
        config=config.replace('https://api.example.com/v1',f'http://127.0.0.1:{fixture.port}/v1').replace('model = "auto"','model = "preflight-mock"')
        config=config.replace('http://127.0.0.1:8090',f'http://127.0.0.1:{fixture.port}').replace('http://127.0.0.1:7474',graph_url or (run.args.graph if direct_graph else f'http://127.0.0.1:{fixture.port}'))
        config=config.replace('context_tokens = 100000',f'context_tokens = {context}').replace('max_tokens = 1200','max_tokens = 512').replace('timeout_seconds = 45',f'timeout_seconds = {model_timeout}')
        config=config.replace('extract_every_tokens = 2048',f'extract_every_tokens = {extract}').replace('poll_seconds = 2','poll_seconds = 1').replace('max_attempts = 8',f'max_attempts = {attempts}')
        config=config.replace('retrieval_timeout_seconds = 8',f'retrieval_timeout_seconds = {retrieval_timeout}').replace('reconcile_updates = true',f'reconcile_updates = {str(reconcile).lower()}')
        config += f'\n' # credentials belong to Config root, not the last section
        self.config=self.root/'config.toml'; self.config.write_text(config); self.config.chmod(0o600)
        self.process=None; self.samples=[]; self.monitor_stop=threading.Event(); self.busy=0; self.rate_limited=0; self.count_lock=threading.Lock()

    def start(self):
        env={k:v for k,v in os.environ.items() if not k.startswith('MNEME_')}
        env.update(MNEME_PREFLIGHT_ROOT=str(self.root),MNEME_PREFLIGHT_CONFIG=str(self.config))
        self.log=(self.root/'server.log').open('a')
        self.process=subprocess.Popen([str(self.run.binary)],cwd=ROOT,env=env,stdout=self.log,stderr=subprocess.STDOUT)
        deadline=time.monotonic()+30
        while True:
            if self.process.poll() is not None: raise RuntimeError(f'isolated server exited: {self.root}/server.log')
            try:
                if requests.get(self.url+'/health/live',timeout=1).ok: break
            except requests.RequestException: pass
            if time.monotonic()>deadline: raise RuntimeError('acceptance server startup timed out')
            time.sleep(.05)
        self.monitor_stop.clear(); self.monitor=threading.Thread(target=self.sample_loop,daemon=True); self.monitor.start()
        return self

    def sql(self,statement,parameters=(),write=False):
        if not self.db.parent.joinpath('.mneme-preflight-owned').is_file() or not self.db.resolve().is_relative_to(self.run.out): raise ValueError('refusing non-owned SQLite path')
        with sqlite3.connect(self.db,timeout=10) as db:
            db.row_factory=sqlite3.Row
            rows=[dict(x) for x in db.execute(statement,parameters)]
            if write: db.commit()
            return rows

    def sample_loop(self):
        proc=psutil.Process(self.process.pid)
        while not self.monitor_stop.wait(.25):
            try:
                io=proc.io_counters(); mem=proc.memory_info(); cpu=proc.cpu_times()
                jobs=self.sql("SELECT status,count(*) n FROM jobs GROUP BY status")
                self.samples.append({'at':time.time(),'rss_bytes':mem.rss,'fds':proc.num_fds(),'threads':proc.num_threads(),'pid':proc.pid,
                                     'cpu_seconds':cpu.user+cpu.system,'read_bytes':io.read_bytes,'write_bytes':io.write_bytes,'jobs':{r['status']:r['n'] for r in jobs}})
            except (psutil.Error,sqlite3.Error): pass

    def request(self,path,body=None,timeout=20):
        start=time.monotonic()
        r=requests.request('GET' if body is None else 'POST',self.url+path,headers={'Authorization':'Bearer '+TOKEN},json=body,timeout=timeout)
        return r,1000*(time.monotonic()-start)

    def chat(self,text,request_id=None):
        request_id=request_id or str(uuid.uuid4()); began=time.monotonic(); attempts=0
        while True:
            response,elapsed=self.request('/v1/chat',{'request_id':request_id,'message':text},timeout=30)
            attempts+=1
            if response.status_code in (409,429) and time.monotonic()-began<90:
                with self.count_lock:
                    if response.status_code==409: self.busy+=1
                    else: self.rate_limited+=1
                time.sleep(.02+random.random()*.03); continue
            if not response.ok or 'event: done' not in response.text:
                raise RuntimeError(f'chat_failed http={response.status_code} outcome={response.text[-180:]}')
            return {'request_id':request_id,'ms':1000*(time.monotonic()-began),'attempts':attempts,'replayed':'"replayed":true' in response.text}

    def age_pending(self):
        # Advance the scheduling clock in this owned test DB, without sleeping 120 s.
        self.sql("UPDATE turns SET completed_at=? WHERE status='complete' AND id IN (SELECT turn_id FROM messages WHERE role='user' AND id NOT IN (SELECT message_id FROM extracted_messages))",(int(time.time())-121,),True)

    def drain(self,timeout=90):
        self.age_pending(); began=time.monotonic()
        while time.monotonic()-began<timeout:
            jobs=self.sql("SELECT status,count(*) n FROM jobs GROUP BY status")
            counts={r['status']:r['n'] for r in jobs}
            raw=self.sql("SELECT count(*) n FROM messages WHERE role='user'")[0]['n']
            done=self.sql("SELECT count(*) n FROM extracted_messages")[0]['n']
            if counts.get('dead',0): return {'ok':False,'jobs':counts,'raw':raw,'receipts':done,'elapsed_ms':1000*(time.monotonic()-began)}
            if not sum(counts.values()) and raw==done: return {'ok':True,'jobs':counts,'raw':raw,'receipts':done,'elapsed_ms':1000*(time.monotonic()-began)}
            time.sleep(.1)
        return {'ok':False,'reason':'drain_timeout','jobs':counts,'raw':raw,'receipts':done}

    def resources(self):
        samples=self.samples
        return {'peak_rss_bytes':max((x['rss_bytes'] for x in samples),default=0),
                'max_fds':max((x['fds'] for x in samples),default=0),'last_fds':samples[-1]['fds'] if samples else None,
                'queue_peak':max((sum(x['jobs'].values()) for x in samples),default=0),
                'running_peak':max((x['jobs'].get('running',0) for x in samples),default=0),
                'cpu_seconds':max((x['cpu_seconds'] for x in samples),default=0),
                'write_bytes':max((x['write_bytes'] for x in samples),default=0),
                'read_bytes':max((x['read_bytes'] for x in samples),default=0),
                'sqlite_bytes':sum(p.stat().st_size for p in self.root.glob('test.sqlite3*') if p.is_file())}

    def stop(self,crash=False):
        if self.process and self.process.poll() is None:
            self.process.kill() if crash else self.process.send_signal(signal.SIGINT)
            try: self.process.wait(timeout=15)
            except subprocess.TimeoutExpired: self.process.kill(); self.process.wait()
        self.monitor_stop.set()
        if hasattr(self,'monitor'): self.monitor.join(2)
        if hasattr(self,'log'): self.log.close()
        write_json(self.root/'resources.json',self.samples)


class Run:
    def __init__(self,args):
        self.id='preflight-'+uuid.uuid4().hex[:12]
        self.out=args.output.resolve(); self.out.mkdir(parents=True,exist_ok=False)
        (self.out/'.mneme-preflight-owned').write_text(self.id)
        self.args=args; self.results={}; self.servers=[]
        self.source_sha=hashlib.sha256(b''.join(p.read_bytes() for p in sorted((ROOT/'src').glob('*.rs')))).hexdigest()
        self.binary=self.out/'preflight-server'
        os.link(ROOT/'target/debug/examples/preflight_server',self.binary)
        self.binary_sha=hashlib.sha256(self.binary.read_bytes()).hexdigest()
        self.service_samples=[]
        try: self.model_health=requests.get(args.models+'/health',timeout=2).json()
        except requests.RequestException: self.model_health=None
        self.model_threads=None
        expected_python=str(args.graph_owner_file.resolve().parent/'venv/bin/python')
        for proc in psutil.process_iter(['cmdline']):
            cmd=proc.info['cmdline'] or []
            if cmd and cmd[0]==expected_python and 'uvicorn' in cmd and args.models.rsplit(':',1)[1] in cmd:
                try: self.model_threads=int(proc.environ().get('MNEME_CPU_THREADS','3'))
                except (psutil.Error,ValueError): pass
        self.fixture=Fixture(args.graph,args.models,args.provider_file,self.out).start()
        self.graph("CREATE (:MnemePreflight {id:$id})",{'id':self.id},allow_new=True)
        self.persist()

    def graph(self,statement,params=None,allow_new=False):
        if not allow_new:
            response=requests.post(self.args.graph+'/db/neo4j/tx/commit',auth=('neo4j',GRAPH_PASSWORD),json={'statements':[{'statement':'MATCH (e:MnemePreflight {id:$id}) RETURN e.id','parameters':{'id':self.id}}]},timeout=15).json()
            if not response.get('results',[{}])[0].get('data'): raise RuntimeError('missing Neo4j isolation marker')
        r=requests.post(self.args.graph+'/db/neo4j/tx/commit',auth=('neo4j',GRAPH_PASSWORD),json={'statements':[{'statement':statement,'parameters':params or {}}]},timeout=60)
        r.raise_for_status(); value=r.json()
        if value.get('errors'): raise RuntimeError('test Neo4j rejected Cypher: '+str(value['errors'])[:500])
        return [r['row'] for r in value['results'][0]['data']]

    def reset_graph(self):
        # Only test labels in a marker-verified, newly owned graph.
        self.graph('MATCH (m:Memory) WITH m LIMIT 20000 DETACH DELETE m')
        self.graph('MATCH (e:Entity) WHERE NOT (e)--() DELETE e')

    def server(self,name,**kwargs):
        self.reset_graph(); s=Server(self,name,self.fixture,**kwargs).start(); self.servers.append(s); return s

    def service_resources(self):
        owner=json.loads(self.args.graph_owner_file.read_text())
        result={}
        for entry in owner.get('processes',[]):
            try:
                proc=psutil.Process(entry['pid']); mem=proc.memory_info(); cpu=proc.cpu_times(); io=proc.io_counters()
                result['neo4j']={'rss_bytes':mem.rss,'fds':proc.num_fds(),'threads':proc.num_threads(),'cpu_seconds':cpu.user+cpu.system,'read_bytes':io.read_bytes,'write_bytes':io.write_bytes}
            except psutil.Error: pass
        for proc in psutil.process_iter(['pid','cmdline','memory_info']):
            cmd=proc.info['cmdline'] or []
            if cmd and '/tmp/mneme-preflight-runtime/' in cmd[0] and 'uvicorn' in cmd and '19090' in cmd:
                try:
                    cpu=proc.cpu_times(); io=proc.io_counters()
                    result['models']={'rss_bytes':proc.info['memory_info'].rss,'fds':proc.num_fds(),'threads':proc.num_threads(),'cpu_seconds':cpu.user+cpu.system,'read_bytes':io.read_bytes,'write_bytes':io.write_bytes}
                except psutil.Error: pass
        return result

    def persist(self):
        write_json(self.out/'results.json',{'run':self.id,'host':{'cpus':os.cpu_count(),'ram_bytes':psutil.virtual_memory().total},
                   'source_sha256':self.source_sha,'binary_sha256':self.binary_sha,
                   'mode':'mock_pressure_real_neo4j_plus_separate_live_samples','model_health':self.model_health,'model_threads':self.model_threads,'results':self.results})

    def check(self,name,row):
        self.service_samples.append({'at':time.time(),'phase':name,**self.service_resources()})
        write_json(self.out/'service-resources.json',self.service_samples)
        self.results[name]=row; self.persist(); print(name,json.dumps(row,ensure_ascii=False)[:1800],flush=True)

    def long(self):
        cells=[]
        for target in (10_000,50_000,100_000):
            s=self.server(f'long-{target}',direct_graph=True); before=len(self.fixture.extractions); records=[]; total=0; start=time.monotonic()
            while total<target:
                i=len(records)
                text=f'本轮长对话{target}第{i}轮，我今天玩了终末地，剧情让我想继续讨论。\n填充资料'+(' personal conversation detail'*110)+f'。\n收尾标记 END-{target}-{i}，我还想记住今晚看到了晚霞。'
                record=s.chat(text); record.update(text=text,sha256=hashlib.sha256(text.encode()).hexdigest()); records.append(record)
                total+=len(ENC.encode(text))+len(ENC.encode('我听到了，继续说吧。'))
            burst_seconds=time.monotonic()-start
            raw=s.sql("SELECT u.id,u.content,a.content assistant FROM messages u JOIN messages a ON a.turn_id=u.turn_id AND a.role='assistant' WHERE u.role='user' ORDER BY u.seq")
            expected_groups=[]; tokens=0; ids=[]
            for r in raw:
                tokens+=len(ENC.encode(r['content']))+len(ENC.encode(r['assistant'])); ids.append(r['id'])
                if tokens>=2048: expected_groups.append({'tokens':tokens,'sources':ids}); tokens=0; ids=[]
            drain=s.drain(); rows=s.sql('SELECT * FROM extraction_runs ORDER BY seq')
            seen=[x for e in self.fixture.extractions[before:] for x in e['source_ids']]
            counts=Counter(seen)
            literal=s.sql('SELECT s.quote,m.content FROM memory_sources s JOIN messages m ON m.id=s.message_id')
            row={'target_tokens':target,'actual_tokens':total,'turns':len(records),'tokens_per_second':total/burst_seconds,
                 'messages_per_second':len(records)/burst_seconds,**percentiles([r['ms'] for r in records]),
                 'raw_intact':len(raw)==len(records) and [hashlib.sha256(r['content'].encode()).hexdigest() for r in raw]==[r['sha256'] for r in records],
                 'expected_threshold_batches':len(expected_groups),'observed_threshold_batches':sum(r['input_tokens']>=2048 for r in rows),
                 'unexpected_recompression_sources':sum(n-1 for n in counts.values() if n>1),'missing_sources':len({r['id'] for r in raw}-set(seen)),
                 'source_quotes_literal':all(r['quote'] in r['content'] for r in literal),'drain':drain,'resources':s.resources(),
                 'context_compactions':(s.sql('SELECT compactions FROM conversation_context') or [{'compactions':0}])[0]['compactions'],
                 'active_episodes':s.sql("SELECT count(*) n FROM memories WHERE status='active' AND tier='episodic'")[0]['n']}
            write_json(s.root/'workload.json',{'records':records,'extraction_runs':rows,'expected_groups':expected_groups})
            cells.append(row); s.stop(); self.check('long_conversations',cells)

    def concurrent(self):
        cells=[]
        for concurrency in (1,4,16):
            s=self.server(f'concurrency-{concurrency}',direct_graph=True); start=time.monotonic(); search_rows=[]; stop=threading.Event()
            def searcher():
                while not stop.is_set():
                    try:
                        r,ms=s.request('/v1/console/retrieval',{'query':'终末地剧情的讨论'},timeout=10)
                        search_rows.append({'ms':ms,'http_status':r.status_code})
                    except requests.RequestException: search_rows.append({'error':True})
                    stop.wait(.05)
            search=threading.Thread(target=searcher); search.start()
            texts=[f'并发{concurrency}第{i}条：终末地的任务 CONCURRENT-{concurrency}-{i}，我喜欢聊剧情。' for i in range(64)]
            with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool: rows=list(pool.map(s.chat,texts))
            seconds=time.monotonic()-start; drain=s.drain(); stop.set(); search.join(12)
            replay=[s.chat(texts[i],rows[i]['request_id']) for i in range(0,64,8)]
            raw=s.sql("SELECT content,count(*) n FROM messages WHERE role='user' GROUP BY content")
            row={'concurrency':concurrency,'messages':64,'throughput_messages_per_second':64/seconds,**percentiles([r['ms'] for r in rows]),
                 'serialization_409_retries':s.busy,'admission_429_retries':s.rate_limited,'raw_exactly_once':len(raw)==64 and all(r['n']==1 for r in raw),
                 'duplicate_requests_replayed':all(r['replayed'] for r in replay),'concurrent_searches':len(search_rows),
                 'search_errors':sum(x.get('http_status',0)!=200 for x in search_rows),'search_latency':percentiles([x['ms'] for x in search_rows if 'ms'in x]),
                 'sqlite_integrity':s.sql('PRAGMA integrity_check'),'drain':drain,'resources':s.resources()}
            cells.append(row); s.stop(); self.check('concurrency',cells)

    def faults(self):
        cells=[]
        for fault in ('embedding_timeout','embedding_unavailable','graph_unavailable','graph_tcp_reset','reranker_invalid','invalid_llm','pause_llm'):
            tcp=TcpProxy(self.args.graph).start() if fault=='graph_tcp_reset' else None
            s=self.server('fault-'+fault,extract=1,graph_url=f'http://127.0.0.1:{tcp.port}' if tcp else None,attempts=8 if tcp else 3)
            baseline=s.chat('基线记录：我在测试项目中主要讨论终末地剧情。'); s.drain()
            self.fixture.fault=fault
            if tcp: tcp.suspend()
            if fault=='pause_llm': self.fixture.pause.clear()
            rows=[]
            for i in range(3): rows.append(s.chat(f'故障{fault}第{i}条，今天看到了晚霞，标记 FAULT-{fault}-{i}。'))
            if fault=='pause_llm':
                deadline=time.monotonic()+8
                while time.monotonic()<deadline and not s.sql("SELECT id FROM jobs WHERE status='running'"): time.sleep(.02)
                s.stop(crash=True)
                self.fixture.fault=None; self.fixture.pause.set(); s.start()
            elif fault=='invalid_llm':
                time.sleep(2.2)
            elif fault in ('embedding_timeout','embedding_unavailable','graph_unavailable'):
                time.sleep(1.2)
            self.fixture.fault=None
            if tcp: tcp.resume()
            recovery_start=time.monotonic(); drain=s.drain(timeout=90)
            raw=s.sql("SELECT count(*) n,count(DISTINCT turn_id) distinct_turns FROM messages WHERE role='user'")[0]
            row={'fault':fault,'chat_latency':percentiles([r['ms'] for r in rows]),'chat_completed':len(rows),
                 'raw_preserved':raw['n']==4 and raw['distinct_turns']==4,'recovery_ms':1000*(time.monotonic()-recovery_start),
                 'drain':drain,'memory_source_duplicates':s.sql('SELECT count(*) n FROM (SELECT memory_id,message_id,count(*) n FROM memory_sources GROUP BY memory_id,message_id HAVING n>1)')[0]['n'],
                 'indexed_active':s.sql("SELECT count(*) n FROM memories WHERE status='active' AND indexed_revision=revision")[0]['n'],
                 'resources':s.resources()}
            cells.append(row); s.stop();
            if tcp: tcp.stop()
            self.check('faults',cells)

    def quality(self,live=False):
        data=json.loads((ROOT/'tests/fixtures/preflight/dialogues.json').read_text())
        cases=data['cases'] if not live else [c for c in data['cases'] if c['id'].endswith(('-00','-01'))]
        s=self.server('quality-live' if live else 'quality-mock',extract=100_000,context=100000,model_timeout=45 if live else 1,retrieval_timeout=8 if live else 3,reconcile=live,direct_graph=True)
        self.fixture.mode='live' if live else 'mock'
        start=time.monotonic(); mapping={}
        for case in cases:
            ids=[]
            for text in case['turns']:
                record=s.chat(text)
                ids.extend(r['id'] for r in s.sql("SELECT id FROM messages WHERE turn_id=? AND role='user'",(record['request_id'],)))
            mapping[case['id']]=ids
        drain=s.drain(timeout=480 if live else 90)
        evidence=s.sql('SELECT m.id,m.content,m.kind,m.tier,m.evidence_type,m.confidence,m.status,s.message_id,s.quote,u.content original FROM memories m JOIN memory_sources s ON s.memory_id=m.id JOIN messages u ON u.id=s.message_id')
        rows=[]
        for case in cases:
            stored=[r for r in evidence if r['message_id'] in mapping[case['id']]]
            content='\n'.join(r['content'] for r in stored)
            covered=[all(term in content for term in clause) for clause in case['expected_clauses']]
            attribution_errors=[r for r in stored if r['evidence_type']=='explicit_fact' and any(x in r['content'] for x in case['forbidden_assertions'])]
            uncertain_ok=not case['uncertain'] or all(r['tier']=='episodic' and r['evidence_type']!='explicit_fact' for r in stored)
            rows.append({'id':case['id'],'category':case['category'],'clauses':len(covered),'covered':sum(covered),
                         'literal_quotes':all(r['quote'] in r['original'] for r in stored),'wrong_attribution':len(attribution_errors),
                         'uncertainty_preserved':uncertain_ok,'empty_ack_ok':not case['expect_empty'] or not stored,'stored':stored})
        result={'mode':'real_deepseek_flash' if live else 'scripted_literal_fixture_not_semantic_accuracy',
                'groups':len(cases),'source_turns':sum(len(c['turns']) for c in cases),'seconds':time.monotonic()-start,
                'gold_clause_coverage':sum(r['covered'] for r in rows)/max(1,sum(r['clauses'] for r in rows)),
                'wrong_attribution_flags':sum(r['wrong_attribution'] for r in rows),
                'literal_quote_failures':sum(not r['literal_quotes'] for r in rows),'uncertainty_failures':sum(not r['uncertainty_preserved'] for r in rows),
                'ack_failures':sum(not r['empty_ack_ok'] for r in rows),'drain':drain,'rows':rows,'resources':s.resources()}
        write_json(self.out/('quality-live.json' if live else 'quality-mock.json'),result)
        self.check('quality_live' if live else 'quality_mock',{k:v for k,v in result.items() if k!='rows'})
        self.fixture.mode='mock'; s.stop()

    def capacity(self,real_reranker=False):
        cells=[]
        self.fixture.mode='real_reranker' if real_reranker else 'mock'
        for size in (1000,10000):
            s=self.server('capacity-'+str(size),direct_graph=True,model_timeout=45 if real_reranker else 1,retrieval_timeout=8 if real_reranker else 3); seed=time.monotonic(); rows=[]
            # Capacity phase uses deterministic vectors. Real-model throughput is measured separately.
            with sqlite3.connect(s.db,timeout=10) as db:
                for i in range(size):
                    mid=str(uuid.uuid4()); content=f'档案 ITEM-{i:05}：用户的长期技术项目编号是工程-{i:05}，主题为 Rust 数据检索。'
                    vec=vector(content)
                    db.execute("INSERT INTO memories(id,slot,content,content_hash,kind,importance,confidence,entities,embedding,status,created_at,updated_at,source_seq,tier,evidence_type,origin) VALUES(?,?,?,?,'fact',0.8,0.99,'[]',?,'active',?,?,0,'long_term','explicit_fact','manual')",(mid,'capacity.'+mid,content,hashlib.sha256(content.encode()).hexdigest(),json.dumps(vec),int(time.time()),int(time.time())))
                    rows.append({'id':mid,'content':content,'embedding':vec})
                    if len(rows)==128 or i==size-1:
                        self.graph("UNWIND $rows AS r CREATE (m:Memory:ActiveMemory {id:r.id,slot:r.id,content:r.content,embedding:r.embedding,revision:1,updated_at:timestamp()/1000})",{'rows':rows}); rows=[]
                db.commit()
            # Ensure indexes exist through the real projection path.
            s.chat('容量初始化：今天继续讨论技术项目。'); s.drain()
            seed_seconds=time.monotonic()-seed
            stage=[]
            for i in range(24):
                query=f'档案 ITEM-{(i*397)%size:05} 的长期技术项目编号是什么？'
                r,ms=s.request('/v1/console/retrieval',{'query':query},timeout=15)
                body=r.json(); trace=body.get('trace',{})
                wanted=f'ITEM-{(i*397)%size:05}'
                candidate_ids={c['id'] for c in trace.get('candidates',[])}
                wanted_id=s.sql('SELECT id FROM memories WHERE content LIKE ?',('%'+wanted+'%',))[0]['id']
                stage.append({'query':query,'ms':ms,'http_status':r.status_code,'degraded':body.get('degraded'),
                              'target_in_first_stage':wanted_id in candidate_ids,'target_in_final':any(wanted in x['memory']['content'] for x in body.get('results',[])),
                              'candidate_count':len(trace.get('candidates',[])),'result_count':len(body.get('results',[]))})
            row={'memories':size,'seed_seconds':seed_seconds,'total_seconds':time.monotonic()-seed,'retrievals':len(stage),**percentiles([x['ms'] for x in stage]),
                 'errors':sum(x['http_status']!=200 for x in stage),'degraded':sum(bool(x['degraded']) for x in stage),
                 'target_stage_hits':sum(x['target_in_first_stage'] for x in stage),'target_final_hits':sum(x['target_in_final'] for x in stage),
                 'graph_nodes':self.graph('MATCH (m:Memory) RETURN count(m)')[0][0],
                 'resources':s.resources(),'node_vectors':'deterministic lexical mock; not semantic quality evidence','reranker':'real_gte_multilingual_base' if real_reranker else 'mock_lexical_saturating',
                 'neo4j_store_bytes':sum(p.stat().st_size for p in (Path(json.loads(self.args.graph_owner_file.read_text()).get('data_root',str(Path(json.loads(self.args.graph_owner_file.read_text())['root'])/'data')))/'databases/neo4j').rglob('*') if p.is_file()),
                 'neo4j_index_bytes':sum(p.stat().st_size for p in (Path(json.loads(self.args.graph_owner_file.read_text()).get('data_root',str(Path(json.loads(self.args.graph_owner_file.read_text())['root'])/'data')))/'databases/neo4j/schema/index').rglob('*') if p.is_file())}
            write_json(s.root/'retrievals.json',stage); cells.append(row); s.stop(); self.check('capacity',cells)

    def retrieval(self,real=False):
        data=json.loads((ROOT/'tests/fixtures/memory/retrieval.json').read_text())
        s=self.server('retrieval-real' if real else 'retrieval-mock',model_timeout=45 if real else 1,retrieval_timeout=8 if real else 3,direct_graph=True)
        self.fixture.mode='live_models' if real else 'mock'
        docs=list(data['memories'])
        for i in range(100): docs.append({'id':f'distractor-{i}','content':f'其他项目资料{i}：海边城市的咖啡店讨论程序语言、摄影与旅行，未说明用户个人资料。','entities':[]})
        texts=[m['content'] for m in docs]; vectors=[]; began=time.monotonic(); embed_times=[]
        for i in range(0,len(texts),8):
            at=time.monotonic(); r=requests.post(f'http://127.0.0.1:{self.fixture.port}/embed',json={'texts':texts[i:i+8],'query':False},timeout=120); r.raise_for_status(); vectors.extend(r.json()['embeddings']); embed_times.append(1000*(time.monotonic()-at))
        embed_seconds=time.monotonic()-began
        for m,v in zip(docs,vectors):
            mid=str(uuid.uuid4()); m['storage_id']=mid
            expired=m.get('expired',False); deleted=m.get('deleted',False); status=m.get('status','forgotten' if deleted else 'active')
            s.sql("INSERT INTO memories(id,slot,content,content_hash,kind,importance,confidence,entities,embedding,status,created_at,updated_at,source_seq,valid_until,deleted_at) VALUES(?,?,?,?,'fact',0.9,0.99,?,?,?,?,?,0,?,?)",(mid,mid,m['content'],hashlib.sha256(m['content'].encode()).hexdigest(),json.dumps(m['entities'],ensure_ascii=False),json.dumps(v),status,int(time.time()),int(time.time()),int(time.time())-1 if expired else None,int(time.time()) if deleted else None),True)
            if status=='active' and not expired and not deleted:
                self.graph("CREATE (m:Memory:ActiveMemory {id:$id,slot:$id,content:$content,embedding:$embedding,revision:1,updated_at:timestamp()/1000})",{'id':mid,'content':m['content'],'embedding':v})
                for entity in m['entities']:
                    self.graph('MATCH (m:Memory {id:$id}) MERGE (e:Entity {name:$name}) MERGE (m)-[:MENTIONS]->(e)',{'id':mid,'name':entity})
        # Initialize indexes via an ordinary projection, then exclude that synthetic helper in evaluation.
        self.fixture.mode='mock'; s.chat('本次是隔离召回验收，标记 INDEX-HELPER。'); s.drain(); self.fixture.mode='live_models' if real else 'mock'
        identity={m['storage_id']:m['id'] for m in docs}; by_mode={x:[] for x in ('vector','fulltext','hybrid','hybrid_reranker','production_hybrid','production_reranker')}; rows=[]
        for q in data['queries']:
            at=time.monotonic(); response,ms=s.request('/v1/console/retrieval',{'query':q['query']},timeout=30); response.raise_for_status(); result=response.json(); trace=result['trace']; channels={c['name']:c['hits'] for c in trace['channels']}
            # Independent first-stage probes: a production deadline may discard
            # its debug trace and must not be misreported as an ANN/fulltext miss.
            embedding=requests.post(f'http://127.0.0.1:{self.fixture.port}/embed',json={'texts':[q['query']],'query':True},timeout=120)
            embedding.raise_for_status(); qvector=embedding.json()['embeddings'][0]
            raw_vectors=self.graph("CALL db.index.vector.queryNodes('memory_vector',$limit,$vector) YIELD node,score RETURN node.id,score ORDER BY score DESC",{'limit':30,'vector':qvector})
            raw_text=self.graph("CALL db.index.fulltext.queryNodes('memory_text',$query,{limit:$limit}) YIELD node,score RETURN node.id,score ORDER BY score DESC",{'limit':30,'query':lucene(q['query'])})
            vector_ids=[r[0] for r in raw_vectors if r[0] in identity]
            text_ids=[r[0] for r in raw_text if r[0] in identity]
            scores={}
            for ranking in (vector_ids,text_ids):
                for rank,mid in enumerate(ranking,1): scores[mid]=scores.get(mid,0)+1/(60+rank)
            hybrid=sorted(scores,key=lambda mid:(-scores[mid],mid))[:30]
            if hybrid:
                records={r['id']:r['content'] for r in s.sql('SELECT id,content FROM memories')}
                rr=requests.post(f'http://127.0.0.1:{self.fixture.port}/rerank',json={'query':q['query'],'documents':[records[mid] for mid in hybrid]},timeout=120); rr.raise_for_status(); rs=rr.json()['scores']; highest=max(rs)
                reranked=[mid for score,mid in sorted(zip(rs,hybrid),reverse=True) if score>=.1 and score>=.8*highest]
            else: reranked=[]
            production_stage=[c['id'] for c in sorted(trace['candidates'],key=lambda x:(-x.get('rrf_score',0),x['id'])) if c['id'] in identity][:30]
            variants={'vector':vector_ids[:30],'fulltext':text_ids[:30],'hybrid':hybrid,'hybrid_reranker':reranked[:5],
                      'production_hybrid':production_stage,'production_reranker':[r['memory']['id'] for r in result['results'] if r['memory']['id'] in identity]}
            row={'query':q['query'],'kind':q['kind'],'gold':q['relevant'],'production_ms':ms,'degraded':result['degraded'],'trace':trace,'variants':{}}
            for mode,ids in variants.items():
                ranked=[identity[mid] for mid in ids]; m=metric(ranked,q['relevant']);
                if mode=='production_hybrid' and trace.get('reranker_state')=='deadline_exceeded' and not trace.get('channels'):
                    m['recall_at_30']=None; m['mrr']=None
                by_mode[mode].append(m); row['variants'][mode]={'ids':ranked,**m}
            rows.append(row)
            write_json(self.out/('retrieval-real.json' if real else 'retrieval-mock.json'),{'partial':True,'rows':rows})
        summary={'mode':'real_pinned_embedding_reranker_real_neo4j' if real else 'mock_lexical_models_real_neo4j_not_semantic_accuracy',
                 'documents':len(docs),'queries':len(rows),'positive_queries':sum(bool(q['relevant']) for q in data['queries']),
                 'embedding_documents_per_second':len(docs)/embed_seconds,'embedding_batch_latency':percentiles(embed_times),
                 'metrics':{name:aggregate(values) for name,values in by_mode.items()},'production_latency':percentiles([r['production_ms'] for r in rows]),
                 'degraded_queries':sum(r['degraded'] for r in rows),'resources':s.resources()}
        write_json(self.out/('retrieval-real.json' if real else 'retrieval-mock.json'),{'summary':summary,'rows':rows})
        self.check('retrieval_real' if real else 'retrieval_mock',summary); self.fixture.mode='mock'; s.stop()

    def embedding_speed(self):
        self.fixture.mode='live_models'
        documents=[f'用户计划周六去杭州看展，第{i}号计划记录，验收标识 {self.id}-{i}。' for i in range(32)]
        rows=[]
        for label in ('cold_unique_documents','warm_identical_documents'):
            began=time.monotonic(); times=[]; tokens=0; cached=0
            for start in range(0,32,8):
                at=time.monotonic(); response=requests.post(f'http://127.0.0.1:{self.fixture.port}/embed',json={'texts':documents[start:start+8],'query':False},timeout=120)
                response.raise_for_status(); value=response.json(); times.append(1000*(time.monotonic()-at))
                if len(value['embeddings'])!=len(documents[start:start+8]) or any(len(v)!=1024 for v in value['embeddings']): raise RuntimeError('invalid real embedding dimensions')
                usage=value.get('usage',{}); tokens+=usage.get('prompt_tokens',0); cached+=usage.get('prompt_tokens_details',{}).get('cached_tokens',0)
            elapsed=time.monotonic()-began
            rows.append({'workload':label,'documents':32,'seconds':elapsed,'documents_per_second':32/elapsed,'input_tokens':tokens,'cached_tokens':cached,'cache_hit_fraction':cached/tokens if tokens else None,**percentiles(times)})
        self.check('embedding_speed',{'model':'Qwen3-Embedding-0.6B','dtype':(self.model_health or {}).get('dtype'),'cpu_threads':self.model_threads,'rows':rows})
        self.fixture.mode='mock'

    def close(self):
        self.fixture.fault=None; self.fixture.pause.set()
        for s in self.servers:
            if s.process and s.process.poll() is None: s.stop()
        write_json(self.out/'extraction-requests.json',self.fixture.extractions)
        self.fixture.stop(); self.persist()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--graph',default='http://127.0.0.1:18474')
    parser.add_argument('--graph-owner-file',type=Path,default=Path('/tmp/mneme-preflight-runtime/graph-owner.json'))
    parser.add_argument('--models',default='http://127.0.0.1:19090')
    parser.add_argument('--provider-file',type=Path)
    parser.add_argument('--phases',default='long,concurrent,faults,quality,capacity,retrieval')
    args=parser.parse_args()
    for url in (args.graph,args.models):
        if not re.fullmatch(r'http://127\.0\.0\.1:\d+',url): parser.error('only owned loopback upstreams are allowed')
    if not args.graph_owner_file.is_file(): parser.error('missing isolated Neo4j owner file')
    owner=json.loads(args.graph_owner_file.read_text())
    if owner.get('url')!=args.graph or not Path(owner.get('root','')).is_dir(): parser.error('Neo4j ownership mismatch')
    owned=False
    for entry in owner.get('processes',[]):
        try:
            cmd=psutil.Process(entry['pid']).cmdline()
            owned |= 'org.neo4j.server.CommunityEntryPoint' in cmd and any(owner['root'] in a for a in cmd)
        except psutil.Error: pass
    if not owned: parser.error('owned Neo4j process is no longer running')
    run=Run(args)
    try:
        for phase in args.phases.split(','):
            getattr(run,{'quality_live':'quality','retrieval_real':'retrieval','capacity_reranker':'capacity'}.get(phase,phase))(**({'live':True} if phase=='quality_live' else {'real':True} if phase=='retrieval_real' else {'real_reranker':True} if phase=='capacity_reranker' else {}))
    except Exception as error:
        run.check('runner_failure',{'type':type(error).__name__,'message':str(error)[:500]})
        raise
    finally: run.close()


if __name__=='__main__': main()
