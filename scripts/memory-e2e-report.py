#!/usr/bin/env python3
"""Summarize measured JSON, retaining every failure and raw result reference."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re


def percentiles(values):
    values=sorted(values)
    if not values:return {}
    return {key:values[max(0,math.ceil(len(values)*q)-1)] for key,q in [('p50',.5),('p95',.95),('p99',.99)]}|{'n':len(values),'mean':sum(values)/len(values)}


def size(text):
    m=re.search(r'([\d.]+)\s*(GB|GiB|MB|MiB|kB|KiB|B)',text)
    return float(m[1])*{'GB':1e9,'GiB':2**30,'MB':1e6,'MiB':2**20,'kB':1000,'KiB':1024,'B':1}[m[2]] if m else 0


def summarize(directory):
    root=Path(directory);data=json.loads((root/'results.json').read_text())
    profiles=data['profiles']
    gold=json.loads((Path(__file__).resolve().parents[1]/'tests/fixtures/memory/e2e.json').read_text())
    unseen=json.loads((Path(__file__).resolve().parents[1]/'tests/fixtures/memory/e2e-unseen.json').read_text())
    confirm=json.loads((Path(__file__).resolve().parents[1]/'tests/fixtures/memory/e2e-confirm.json').read_text())
    by_id={p['id']:p for p in gold['profiles']+unseen['profiles']+confirm['profiles']}
    composition_path=Path(__file__).resolve().parents[1]/'tests/fixtures/memory/e2e-compositions.json'
    compositions=json.loads(composition_path.read_text())
    def matches(text,fact):
        text=text.replace('我','用户')
        return all(any(word in text for word in group) for group in fact['all'])
    for profile in profiles:
        def clauses(fact):
            return compositions.get(profile['id']+':'+fact['label'],[fact])
        def covers(texts,fact):
            return all(any(matches(text,clause) for text in texts) for clause in clauses(fact))
        def supports(text,fact):
            return any(matches(text,clause) for clause in clauses(fact))
        def duplicates(texts):
            normalized=[re.sub(r'\s+','',t.replace('我','用户')) for t in texts]
            return len(normalized)-len(set(normalized))
        facts=by_id[profile['id']]['final']
        current_gold={}
        for t in profile['turns']:
            expected=t['gold']['facts'];texts=[w[2] for w in t['writes']]
            t['tp']=sum(covers(texts,f) for f in expected)
            t['fp']=sum(not any(supports(text,f) for f in expected) for text in texts)
            t['precision_tp']=len(texts)-t['fp']
            t['fn']=len(expected)-t['tp']
            t['duplicate_facts']=duplicates(texts)
            t['exact']=t['fn']==0 and t['fp']==0 and t['duplicate_facts']==0
            for fact in expected:
                current_gold[re.sub(r'_old$','',fact['label'])]=fact
            if 'active_state' in t:
                state_text=[m['content'] for m in t['active_state']]
                state_tp=sum(covers(state_text,f) for f in current_gold.values())
                state_fp=sum(not any(supports(text,f) for f in current_gold.values()) for text in state_text)
                t['evolution']={'tp':state_tp,'expected':len(current_gold),'fp':state_fp,'exact':state_tp==len(current_gold) and state_fp==0 and duplicates(state_text)==0}
        memories=profile['final_memories']
        profile['final_tp']=sum(covers([m['content'] for m in memories],f) for f in facts)
        profile['final_fp']=sum(not any(supports(m['content'],f) for f in facts) for m in memories)
        profile['final_duplicates']=duplicates([m['content'] for m in memories])
        for q in profile['queries']:
            selected=set();irrelevant=0;relevant_entries=0
            expected=set(q['gold']['relevant'])
            for m in q['selected_memories']:
                labels={f['label'] for f in facts if supports(m['content'],f)}
                relevant_entries+=bool(labels&expected);irrelevant+=not bool(labels&expected)
            selected={f['label'] for f in facts if covers([m['content'] for m in q['selected_memories']],f)}
            q.update(selected=sorted(selected),tp=len(selected&expected),precision_tp=relevant_entries,fp=irrelevant,fn=len(expected-selected),exact=expected.issubset(selected) and irrelevant==0)
    audit_path=root/'answer-audit.json'
    audit=json.loads(audit_path.read_text()) if audit_path.exists() else None
    if audit:
        assert audit['results_sha256']==hashlib.sha256((root/'results.json').read_bytes()).hexdigest(), 'stale semantic audit'
        for profile in profiles:
            for i,q in enumerate(profile['queries']):
                row=next(r for r in audit['rows'] if r['profile']==profile['id'] and r['query_index']==i)
                q['semantic_answer_ok']=row['correct']
    for profile in profiles:
        previous=0
        for t in profile['turns']:
            dead=t['jobs']['dead'];t['processed']=dead==previous;previous=dead
    turns=[t for p in profiles for t in p['turns']];queries=[q for p in profiles for q in p['queries']]
    def metric(rows):
        tp=sum(r['tp'] for r in rows);fp=sum(r['fp'] for r in rows);fn=sum(r['fn'] for r in rows)
        precision_tp=sum(r.get('precision_tp',r['tp']) for r in rows)
        return {'n':len(rows),'exact':sum(r['exact'] for r in rows),'tp':tp,'precision_tp':precision_tp,'fp':fp,'fn':fn,'precision':precision_tp/max(1,precision_tp+fp),'recall':tp/max(1,tp+fn),'false_memory_fraction':fp/max(1,precision_tp+fp)}
    stages={};token_counts={};judgements=[]
    for t in turns:
        for c in t['llm_calls']:
            endpoint=c.get('endpoint','/chat/completions')
            if endpoint=='/chat/completions':
                request=c.get('request',{})
                try:
                    payload=json.loads(request['messages'][1]['content'])
                except (KeyError,TypeError,ValueError):payload={}
                stage='judgement' if 'texts' in payload else 'extraction'
                stages.setdefault(stage,[]).append(c['ms'])
                usage=c.get('response',{}).get('usage',{})
                tally=token_counts.setdefault(stage,{'requests':0,'prompt_tokens':0,'completion_tokens':0,'http_failures':0})
                tally['requests']+=1;tally['http_failures']+=c.get('status',200)!=200
                for field in ('prompt_tokens','completion_tokens'):tally[field]+=usage.get(field,0)
                if stage=='judgement':
                    try:
                        d=json.loads(c['response']['choices'][0]['message']['content'])['decisions'][0]
                        keep=d['kind']!='skip' and not d['sensitive']
                        judgements.append({'correct':keep==bool(t['gold']['facts']),'kind':d['kind'],'category':t['gold']['category']})
                    except (KeyError,TypeError,ValueError,IndexError):pass
            stages.setdefault(endpoint,[]).append(c['ms'])
    resources=json.loads((root/'resources.json').read_text());container={};rss=[];available=[];swap=[]
    for r in resources:
        for line in r.get('generation_status',[]):
            if line.startswith('VmRSS:'):rss.append(int(line.split()[1])*1024)
        for line in r.get('host',[]):
            if line.startswith('MemAvailable:'):available.append(int(line.split()[1])*1024)
            if line.startswith('SwapFree:'):swap.append(int(line.split()[1])*1024)
        for c in r.get('containers',[]):
            name=c['name'].rsplit('-',1)[-1];entry=container.setdefault(name,{'peak_bytes':0,'peak_cpu_percent':0})
            entry['peak_bytes']=max(entry['peak_bytes'],size(c['mem_usage'].split('/')[0]));entry['peak_cpu_percent']=max(entry['peak_cpu_percent'],float(c['cpu_percent'].strip('%')))
    result = {'scoring':'v3: Frozen conjunctive facts support atomic storage via evaluator-only e2e-compositions.json; precision per relevant memory entry, recall per gold fact bundle; exact-content duplicates separate. Answer substrings require semantic audit.',
        'scoring_inputs_sha256':{'e2e-compositions.json':hashlib.sha256(composition_path.read_bytes()).hexdigest(),'report.py':hashlib.sha256(Path(__file__).read_bytes()).hexdigest()},
        'path':str(root),'metadata':json.loads((root/'metadata.json').read_text()),'writes':metric(turns)|{'processed':sum(t['processed'] for t in turns),'processed_exact':sum(t['processed'] and t['exact'] for t in turns)},'retrieval':metric(queries),
        'answers':{'n':len(queries),'minimum_gold_pass':sum(q['answer_ok'] for q in queries),'grounded_minimum_pass':sum(q['answer_ok'] and set(q['gold']['relevant']).issubset(q['selected']) for q in queries)},'degraded':sum(q['degraded'] for q in queries),
        'final':{'duplicates':sum(p['final_duplicates'] for p in profiles),'tp':sum(p['final_tp'] for p in profiles),'fp':sum(p['final_fp'] for p in profiles),'expected':sum(p['final_expected'] for p in profiles)},
        'latency_ms':{'worker_including_poll_and_projection':percentiles([t['extraction_and_projection_ms'] for t in turns]),
            'source_judgement':percentiles(stages.get('judgement',[])),'extraction_generation':percentiles(stages.get('extraction',[])),
            'context_document_evidence_embedding_rpc':percentiles(stages.get('/embed',[])),'sqlite_transaction':percentiles([t['sqlite_transaction_ms'] for t in turns if t.get('sqlite_transaction_ms') is not None]),'projection_and_maintenance_neo4j_rpc':percentiles(stages.get('/db/neo4j/tx/commit',[])),
            'retrieval':percentiles([q['recall_ms'] for q in queries]),'query_embedding':percentiles([q['trace']['embedding_ms'] for q in queries]),
            'rerank':percentiles([q['trace']['rerank_ms'] for q in queries]),'context_assembly':percentiles([q['context_ms'] for q in queries]),
            'answer_generation':percentiles([q['generation_ms'] for q in queries])},
        'resources':{'containers':container,'generation_peak_rss_bytes':max(rss,default=0),'minimum_host_available_bytes':min(available,default=0),'swap_free_first_bytes':swap[0] if swap else 0,'swap_free_min_bytes':min(swap,default=0)},
        'write_failures':[{'profile':p['id'],'index':t['index'],'gold':t['gold'],'writes':t['writes']} for p in profiles for t in p['turns'] if not t['exact']],
        'retrieval_failures':[{'profile':p['id'],'query':q['gold']['say'],'expected':q['gold']['relevant'],'selected':q['selected_memories'],'answer':q['answer']} for p in profiles for q in p['queries'] if not q['exact'] or not q['answer_ok']]}

    criteria={
        'processed_exact_ge_90pct':result['writes']['processed_exact']/max(1,len(turns))>=.9,
        'write_precision_ge_95pct':result['writes']['precision']>=.95,
        'write_recall_ge_90pct':result['writes']['recall']>=.9,
        'retrieval_precision_ge_90pct':result['retrieval']['precision']>=.9,
        'retrieval_recall_ge_90pct':result['retrieval']['recall']>=.9,
        'grounded_answers_ge_80pct':result['answers']['grounded_minimum_pass']/max(1,len(queries))>=.8,
        'final_current_facts_ge_90pct_and_no_extra':result['final']['tp']/max(1,result['final']['expected'])>=.9 and result['final']['fp']==0 and result['final']['duplicates']==0,
    }
    result['model_call_counts_and_tokens']=token_counts
    categories={t['gold']['category'] for t in turns}
    result['write_categories']={category:metric([t for t in turns if t['gold']['category']==category]) for category in sorted(categories)}
    checkpoints=[t['evolution'] for t in turns if 'evolution' in t]
    result['evolution']={'checkpoints':len(checkpoints),'exact':sum(c['exact'] for c in checkpoints),
                         'tp':sum(c['tp'] for c in checkpoints),'expected':sum(c['expected'] for c in checkpoints),'fp':sum(c['fp'] for c in checkpoints),
                         'note':'NA for archived evaluators without per-turn active snapshots; first-pass task failures remain separately reported'}
    if audit:
        result['answers']['semantic_audited_pass']=sum(q['semantic_answer_ok'] for q in queries)
        result['answers']['grounded_semantic_pass']=sum(q['semantic_answer_ok'] and set(q['gold']['relevant']).issubset(q['selected']) for q in queries)
        result['answers']['semantic_audit']='answer-audit.json; every answer checked, exact raw-result SHA256 required'
    result['answers']['mechanical_memory_narration']=sum(bool(re.search(r'历史记录|长期记忆里|保存的历史|存下的信息|根据记忆|记忆里的|记下的信息|记录是之前',q['answer'])) for q in queries)
    result['source_judgements']={'n':len(judgements),'correct_keep_decisions':sum(d['correct'] for d in judgements),'rows':judgements}
    result['quality_gate']={'passed':all(criteria.values()),'criteria':criteria}
    return result

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('directories',nargs='+');p.add_argument('--output',type=Path,required=True);args=p.parse_args()
    args.output.write_text(json.dumps([summarize(d) for d in args.directories],ensure_ascii=False,indent=2)+'\n')
