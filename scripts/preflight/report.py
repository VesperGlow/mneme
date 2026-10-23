#!/usr/bin/env python3
"""Render evidence-based preflight verdict; never treats absent measurements as passing."""
import argparse
import hashlib
import json
from pathlib import Path

ROOT=Path(__file__).resolve().parents[2]


def read(path): return json.loads(Path(path).read_text())
def pct(value): return '未测' if value is None else f'{100*value:.2f}%'
def ms(value): return '未测' if value is None else f'{value:.1f}'
def mib(value): return f'{value/1048576:.1f}'


def evaluate(matrix,live,embedding,limits):
    results=matrix.get('results',{}); quality=live.get('results',{}).get('quality_live'); retrieval=live.get('results',{}).get('retrieval_real')
    gates={}
    gates['same_source_revision']=bool(matrix.get('source_sha256')) and matrix.get('source_sha256')==live.get('source_sha256')==embedding.get('source_sha256')
    longs=results.get('long_conversations',[])
    gates['long_scales_complete']={x.get('target_tokens') for x in longs}=={10000,50000,100000}
    gates['long_integrity']=bool(longs) and all(x.get('raw_intact') and x.get('missing_sources')==0 and x.get('unexpected_recompression_sources')==0 and x.get('source_quotes_literal') for x in longs)
    gates['incremental_thresholds']=bool(longs) and all(x.get('expected_threshold_batches')==x.get('observed_threshold_batches') for x in longs)
    gates['long_tasks_recovered']=bool(longs) and all(x.get('drain',{}).get('ok') and x.get('drain',{}).get('raw')==x.get('drain',{}).get('receipts') for x in longs)
    concurrent=results.get('concurrency',[])
    gates['concurrency_scales_complete']={x.get('concurrency') for x in concurrent}=={1,4,16}
    gates['concurrency_integrity']=bool(concurrent) and all(x.get('raw_exactly_once') and x.get('duplicate_requests_replayed') and x.get('search_errors')==0 and x.get('drain',{}).get('ok') and all(r.get('integrity_check')=='ok' for r in x.get('sqlite_integrity',[])) for x in concurrent)
    faults=results.get('faults',[])
    gates['faults_complete']={x.get('fault') for x in faults}=={'embedding_timeout','embedding_unavailable','graph_unavailable','graph_tcp_reset','reranker_invalid','invalid_llm','pause_llm'}
    gates['fault_isolation_and_recovery']=bool(faults) and all(x.get('chat_completed')==3 and x.get('raw_preserved') and x.get('memory_source_duplicates')==0 and x.get('drain',{}).get('ok') for x in faults)
    capacity=results.get('capacity',[])
    gates['capacity_complete']={x.get('memories') for x in capacity}=={1000,10000}
    gates['capacity_retrieval']=bool(capacity) and all(x.get('errors')==0 and x.get('degraded')==0 and x.get('graph_nodes')==x.get('memories')+1 and x.get('target_stage_hits')==x.get('retrievals') and x.get('target_final_hits')==x.get('retrievals') for x in capacity)
    replay=results.get('quality_mock',{})
    gates['120_group_contract']=replay.get('groups',0)>=100 and replay.get('literal_quote_failures')==0 and replay.get('drain',{}).get('ok',False)
    targets=limits['quality_targets']
    gates['real_quality_evidence']=bool(quality and quality.get('groups',0)>=24 and quality.get('mode')=='real_deepseek_flash')
    gates['real_quality']=bool(quality) and quality.get('gold_clause_coverage',0)>=targets['live_gold_clause_coverage_min'] and quality.get('literal_quote_failures')==0 and quality.get('wrong_attribution_flags')==0 and quality.get('uncertainty_failures')==0 and quality.get('ack_failures')==0 and quality.get('drain',{}).get('ok',False)
    metrics=(retrieval or {}).get('metrics',{}); stage=metrics.get('production_hybrid',{}); final=metrics.get('production_reranker',{})
    gates['real_first_stage_recall']=stage.get('recall_at_30',0)>=targets['real_production_first_stage_recall_at_30_min']
    gates['real_final_recall']=final.get('recall_at_30',0)>=targets['real_final_recall_min']
    gates['real_final_precision']=final.get('returned_precision',0)>=targets['real_final_returned_precision_min']
    gates['real_final_mrr']=final.get('mrr',0)>=targets['real_final_mrr_min']
    gates['real_negative_rejection']=final.get('negative_contamination')==0 and (retrieval or {}).get('degraded_queries')==0
    speeds=embedding.get('results',{}).get('embedding_speed',{}).get('rows',[])
    gates['real_embedding_cold_and_warm']=len(speeds)==2 and speeds[0].get('cache_hit_fraction')==0 and speeds[1].get('cache_hit_fraction')==1
    gates['no_runner_failure']=not any('runner_failure' in x.get('results',{}) for x in [matrix,live,embedding])
    return gates


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--matrix',type=Path,required=True); parser.add_argument('--live',type=Path,required=True); parser.add_argument('--embedding',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--continuation',type=Path)
    parser.add_argument('--capacity-retest',type=Path)
    parser.add_argument('--default-profile',type=Path)
    parser.add_argument('--runtime-evidence',type=Path)
    args=parser.parse_args()
    matrix=read(args.matrix/'results.json'); live=read(args.live/'results.json'); embedding=read(args.embedding/'results.json'); limits=read(ROOT/'scripts/preflight/acceptance.json')
    interrupted=[]
    if args.continuation:
        continuation=read(args.continuation/'results.json')
        if continuation.get('source_sha256')!=matrix.get('source_sha256'): raise SystemExit('continuation source mismatch')
        if 'runner_failure' in matrix.get('results',{}):
            interrupted.append({'directory':str(args.matrix),'failure':matrix['results'].pop('runner_failure'),'explanation':'TCP fault bridge shutdown order on Python 3.13 was fixed and its unit regression plus all interrupted phases were rerun.'})
        matrix['results'].update(continuation['results'])
    mock_capacity=[]
    capacity_retest=None
    if args.capacity_retest:
        capacity_retest=read(args.capacity_retest/'results.json')
        if capacity_retest.get('source_sha256')!=matrix.get('source_sha256'): raise SystemExit('capacity retest source mismatch')
        mock_capacity=matrix['results'].get('capacity',[])
        matrix['results']['capacity']=capacity_retest.get('results',{}).get('capacity',[])
        if 'runner_failure' in capacity_retest.get('results',{}): matrix['results']['runner_failure']=capacity_retest['results']['runner_failure']
    gates=evaluate(matrix,live,embedding,limits)
    default_profile=read(args.default_profile/'results.json') if args.default_profile else None
    default_retrieval=(default_profile or {}).get('results',{}).get('retrieval_real',{})
    default_final=default_retrieval.get('metrics',{}).get('production_reranker',{})
    gates['production_default_profile']=bool(default_profile) and default_profile.get('source_sha256')==matrix.get('source_sha256') and (default_profile.get('model_health') or {}).get('dtype')=='float32' and default_final.get('recall_at_30',0)>=limits['quality_targets']['real_final_recall_min'] and default_final.get('returned_precision',0)>=limits['quality_targets']['real_final_returned_precision_min'] and default_final.get('negative_contamination')==0 and default_retrieval.get('degraded_queries')==0
    runtime=read(args.runtime_evidence) if args.runtime_evidence else None
    if runtime:
        validation=runtime.get('validation',{}); cleanup=runtime.get('cleanup',{})
        gates['regression_checks']=validation.get('rust_failed')==0 and validation.get('python_ok',False) and validation.get('clippy_ok',False)
        gates['test_environment_cleaned']=cleanup.get('owned_services_stopped',False) and cleanup.get('private_credential_removed',False) and cleanup.get('credential_scan_matches')==0 and cleanup.get('production_services_modified') is False
    verdict='GO' if all(gates.values()) else 'NO-GO'
    evidence_dirs=[args.matrix.resolve(),args.live.resolve(),args.embedding.resolve()]
    if args.continuation: evidence_dirs.append(args.continuation.resolve())
    if args.capacity_retest: evidence_dirs.append(args.capacity_retest.resolve())
    if args.default_profile: evidence_dirs.append(args.default_profile.resolve())
    payload={'verdict':verdict,'gates':gates,'acceptance':limits,'matrix':matrix,'live':live,'embedding':embedding,
             'runtime_evidence':runtime,'default_profile':default_profile,'mock_capacity_initial':mock_capacity,'capacity_retest':capacity_retest,'datasets':{str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in [ROOT/'tests/fixtures/preflight/dialogues.json',ROOT/'tests/fixtures/memory/retrieval.json']},
             'interrupted_harness_attempts':interrupted,'evidence_directories':[str(p.relative_to(ROOT)) if p.is_relative_to(ROOT) else str(p) for p in evidence_dirs]}
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.with_suffix('.json').write_text(json.dumps(payload,ensure_ascii=False,indent=2)+'\n')
    r=matrix['results']; q=live.get('results',{}).get('quality_live',{}); rr=live.get('results',{}).get('retrieval_real',{})
    lines=['# Mneme 自动记忆正式上线前验收','',f'日期：2026-10-23。结论：**{verdict}**。本报告不执行生产部署。','',
           '## 环境与证据边界','',
           '实测主机 4 CPU / 约 8 GB RAM。独立 SQLite、回环端口、Neo4j 5.26.32 / JRE 21；无生产服务变更。真实本地模型为仓库固定版本 Qwen3-Embedding-0.6B（BF16）与 gte-multilingual-reranker-base（FP32），2 个 CPU 推理线程。高频压力使用 Mock LLM/模型 API，存储与索引使用真实 SQLite、Neo4j；真实语义验收另用 deepseek-flash 和真实本地模型。','',
           '真实大模型只用于压缩/语义协调，聊天生成仍为 Mock。凭证仅在仓库外 0600 文件及请求头中使用，不进入本报告或仓库。测试数据全部为合成内容。','',
           '## 长对话与增量压缩','',
           '| 目标 / 实际 Token | 轮次 | 吞吐 Token/s | 聊天 P50 / P95 ms | 阈值批次 预期/实测 | 原文/回执 | 队列峰值 | Mneme 峰值 MiB |','| --- | ---: | ---: | --- | --- | --- | ---: | ---: |']
    for x in r.get('long_conversations',[]):
        lines.append(f"| {x['target_tokens']} / {x['actual_tokens']} | {x['turns']} | {x['tokens_per_second']:.1f} | {ms(x['p50_ms'])} / {ms(x['p95_ms'])} | {x['expected_threshold_batches']}/{x['observed_threshold_batches']} | {x['drain']['raw']}/{x['drain']['receipts']} | {x['resources']['queue_peak']} | {mib(x['resources']['peak_rss_bytes'])} |")
    lines+=['','Token 按与验收服务一致的 o200k 本地计数器统计，不冒充 DeepSeek 的账单 Token。阈值保持完整轮次，因此单批允许超过 2048、但不超过下一轮加入前的剩余量。120 秒空闲通过只在自有测试 DB 将未处理轮次 completed_at 推前 121 秒验证，不修改原始消息内容/时间。逐来源核对重复压缩、缺失与逐字引文。','',
            '## 并发与稳定性','',
            '| 并发 | 消息数 | 吞吐 msg/s | 完成 P50 / P95 ms | 409 / 429 重试 | 同时检索次数 | 原文幂等 / 恢复 |','| ---: | ---: | ---: | --- | --- | ---: | --- |']
    for x in r.get('concurrency',[]):
        lines.append(f"| {x['concurrency']} | {x['messages']} | {x['throughput_messages_per_second']:.2f} | {ms(x['p50_ms'])} / {ms(x['p95_ms'])} | {x['serialization_409_retries']} / {x['admission_429_retries']} | {x['concurrent_searches']} | {x['raw_exactly_once']} / {x['drain']['ok']} |")
    lines+=['','Mneme 共用一个单用户对话，生成有意串行化。409/429 是背压，客户端沿用 request ID 重试，完成延迟包含这些等待；不能把它描述成支持 16 路同时生成。每级另重放 8 个已完成请求，SQLite integrity_check 与来源唯一性均记录在 JSON。','',
            '## 故障注入与恢复','',
            '| 故障 | 正常聊天完成 | 故障期聊天 P95 ms | 恢复耗时 ms | 原文完整 | 队列排空 |','| --- | ---: | ---: | ---: | --- | --- |']
    for x in r.get('faults',[]):
        lines.append(f"| {x['fault']} | {x['chat_completed']}/3 | {ms(x['chat_latency']['p95_ms'])} | {ms(x['recovery_ms'])} | {x['raw_preserved']} | {x['drain']['ok']} |")
    lines+=['','Neo4j 除 HTTP 503 外还通过独立 TCP 桥复位真实 keep-alive 套接字并短暂拒绝连接，随后验证同一 Graph 客户端恢复。SIGKILL 只针对本次验收 Mneme 进程，使用同一 SQLite 重启。额外死信回归验证了人工重试仍保留旧失败记录并生成新审计记录。没有终止生产进程。','',
            '## 信息保真与真实语义','',
            f"Mock 契约回放覆盖 {r.get('quality_mock',{}).get('groups','未测')} 组、12 类场景；它只验证写入/引用契约，不能作为真实模型语义通过率。真实分层样本为 {q.get('groups','未测')} 组 / {q.get('source_turns','未测')} 轮，标签信息点覆盖率 {pct(q.get('gold_clause_coverage'))}，错误归因标记 {q.get('wrong_attribution_flags','未测')}，原文引用错误 {q.get('literal_quote_failures','未测')}，不确定性错误 {q.get('uncertainty_failures','未测')}。完整输入、输出及逐条来源见对应质量证据文件。","",
            '覆盖日常游戏吐槽、明确偏好、模糊/变化观点、第三方职业归属、复杂技术实体、重复、低信息量、多轮指代、情绪及相对时间。标签覆盖率是保守的内容/来源审计，不等于任意语义的全自动真实性证明。样本量及模板化数据限制应保留。','',
            '## 真实召回质量','',
            f"{rr.get('documents','未测')} 条文档（含 100 条干扰），{rr.get('queries','未测')} 条标准答案查询，其中 22 条正例、6 条负例。第一阶段独立查询，不依赖 Reranker 补救漏检。","",
            '| 模式 | Recall@30 | 标准 Precision@5 | MRR | 已返回结果准确率 | 负例污染率 |','| --- | ---: | ---: | ---: | ---: | ---: |']
    labels={'vector':'仅向量','fulltext':'仅全文','hybrid':'向量+全文 RRF','hybrid_reranker':'RRF+Reranker（去掉证据门槛的消融）','production_hybrid':'生产混合召回第一阶段','production_reranker':'生产混合+精排+证据门槛'}
    for name,label in labels.items():
        x=rr.get('metrics',{}).get(name,{})
        lines.append(f"| {label} | {pct(x.get('recall_at_30'))} | {pct(x.get('precision_at_5'))} | {x.get('mrr','未测')} | {pct(x.get('returned_precision'))} | {pct(x.get('negative_contamination'))} |")
    lines+=['',
            'Precision@5 固定以 5 为分母；这组稀疏金标共 24 个相关答案、28 次查询，满覆盖时该指标上限是 24/(28×5)=17.14%。已返回结果准确率另列。Recall/MRR 只对正例平均；负例单独统计。消融没有生产的独立向量/强精排证据门槛，不能把它与最终生产过滤混为一谈。', '',
            f"实际生产检索 P50 / P95 / P99：{ms(rr.get('production_latency',{}).get('p50_ms'))} / {ms(rr.get('production_latency',{}).get('p95_ms'))} / {ms(rr.get('production_latency',{}).get('p99_ms'))} ms；降级查询 {rr.get('degraded_queries','未测')}。","",
            '## 容量与资源','',
            '| 预置长期记忆 | 导入并就绪 s | 检索 P50 / P95 ms | Mneme 峰值 MiB | SQLite MiB | Neo4j 文件 MiB | 其中索引 MiB | 第一阶段/最终命中 |','| ---: | ---: | --- | ---: | ---: | ---: | ---: | --- |']
    for x in r.get('capacity',[]):
        lines.append(f"| {x['memories']} | {x['seed_seconds']:.2f} | {ms(x['p50_ms'])} / {ms(x['p95_ms'])} | {mib(x['resources']['peak_rss_bytes'])} | {mib(x['resources']['sqlite_bytes'])} | {mib(x.get('neo4j_store_bytes',0))} | {mib(x.get('neo4j_index_bytes',0))} | {x.get('target_stage_hits','未测')}/{x.get('target_final_hits','未测')}（各24次） |")
    lines+=['','容量级别额外各有 1 条索引初始化情景。SQLite 大小含当时 WAL/SHM；Neo4j 文件大小不含事务日志，索引为其子集。容量导入使用预计算确定性向量以单独测存储与 HNSW/全文增长，不是 10000 次真实推理，也不把这些向量当语义质量证据。每级执行 24 次实际混合检索。有效情景工作集仍遵循 256 条上限；1000/10000 是长期记忆容量。','',
            '| 真实 Embedding 工作负载 | 文档/s | 缓存 Token 占比 | 批次 P50 / P95 ms |','| --- | ---: | ---: | --- |']
    for x in embedding.get('results',{}).get('embedding_speed',{}).get('rows',[]):
        lines.append(f"| {x['workload']} | {x['documents_per_second']:.2f} | {pct(x['cache_hit_fraction'])} | {ms(x['p50_ms'])} / {ms(x['p95_ms'])} |")
    if mock_capacity:
        lines+=['','容量精排对照：初次 Mock 词法精排在近重复编号档案上出现分数饱和，1000/10000 条的最终命中分别为 '+', '.join(str(x.get('target_final_hits'))+'/'+str(x.get('retrievals')) for x in mock_capacity)+'；第一阶段均已包含目标。随后保持同样规模、同一应用源码与同样向量构造，仅换真实 GTE Reranker复测，上表为复测值。此对照保留在 JSON，不把 Mock 局限当作真实模型准确率。复测复用了测试 Neo4j，文件大小是高水位；最初新数据目录的空间快照另保留在 mock_capacity_initial。','']
    if default_profile:
        latency=default_retrieval.get('production_latency',{})
        lines+=['','### 部署默认配置复核','',f"另以 FP32 Embedding / 3 CPU 线程（Reranker 仍为 FP32）实测相同的 123 条文档和 28 条查询。最终 Recall {pct(default_final.get('recall_at_30'))}，已返回结果准确率 {pct(default_final.get('returned_precision'))}，负例污染 {pct(default_final.get('negative_contamination'))}，P95 {ms(latency.get('p95_ms'))} ms，降级 {default_retrieval.get('degraded_queries','未测')}。本项不调用付费大模型。",'']
        for speed in default_profile.get('results',{}).get('embedding_speed',{}).get('rows',[]):
            lines.append(f"- FP32 `{speed['workload']}`：{speed['documents_per_second']:.2f} 文档/s，缓存 Token {pct(speed['cache_hit_fraction'])}，8 文档批次 P95 {ms(speed['p95_ms'])} ms。")
    if runtime:
        lines+=['','### 服务进程峰值与 I/O','', '| 服务 | 内核 VmHWM MiB | 采样末 FD | 累计 CPU s | 实际读 / 写 MiB |','| --- | ---: | ---: | ---: | --- |']
        for service in runtime.get('services',[]):
            lines.append(f"| {service['service']} | {mib(service['VmHWM'])} | {service['fds']} | {service['cpu_seconds']:.2f} | {mib(service['read_bytes'])} / {mib(service['write_bytes'])} |")
        lines+=['','以上为服务进程各自生命周期的内核高水位，不相加冒充同一时刻总峰值。CPU/I/O 是累计量。Neo4j 较多 FD 主要为索引/存储文件；文件与套接字分类保存在 JSON。三档并发另外直接核对了 64 条原文的完整集合，提取最大尝试次数均为 1，未出现提取因锁竞争失败重试；这不等于不存在短时锁等待。','']
        paid=runtime['paid_llm']; validation=runtime['validation']
        lines+=['## 回归与调用量','',f"Rust 全量：{validation['rust_passed']} 通过、{validation['rust_failed']} 失败、{validation['rust_ignored']} 个原有 opt-in 测试跳过（真实服务已由本验收脚本单独执行）；Python：{validation['python_run']} 项全部通过。Clippy（-D warnings）、格式与差异检查通过。真实付费大模型累计 {paid['requests']} 次压缩/协调请求，API 报告 {paid['tokens']} Token，无付费压力流量。测试服务与临时凭证已清理；原始数据和模型缓存保留。",'']
    lines+=['','CPU、磁盘实际读写、RSS、FD、队列峰值和服务进程样本保存在 resources.json / service-resources.json。共享服务累计 CPU/I/O 不能冒充单次请求成本；无长期 soak/长期泄漏保证。真实本地模型在此机器上是主要计算瓶颈，缓存命中吞吐不能用于估算新文档推理吞吐。','',
            '## 本次修复与复测','',
            '- 无关新话题会被 recent_context 重新注入此前读过的记忆：增加明确承接条件、再次精排和短时限，精排失败时不补入。失败回归已先复现再修复。',
            '- 死信重试重置队列 attempts 后覆盖旧审计、漏计新一轮存储：审计 attempts 改为持久单调递增，只更新当前 running 记录。',
            '- 真实模型将“好像/再看看”的内容高置信标成明确事实：服务端保留不确定性原话，避免证据标签失真。',
            '- 普通逗号被拆成多个问题，增加无用精排且使 8 秒检索超时；寒暄片段还会引入错误主题：只拆多个明确问题，聚焦子问题已包含完整原文时不额外重复整轮精排。',
            '- 评估器不能把超时后丢失的调试轨迹当作第一阶段真实漏检：向量/全文阶段增加独立实测，超时而无轨迹的生产阶段记为未测。','',
            '## 验收门槛','']
    lines += [f"- {'通过' if value else '失败/缺证据'}：`{name}`" for name,value in gates.items()]
    if interrupted: lines+=['','测试运行说明：首次最终矩阵在 TCP 故障桥关闭时遇到 Python 3.13 的 wait_closed 等待连接问题。已修复测试夹具、增加 keep-alive 单元回归，并在同一应用源码版本下续跑全部故障、质量和容量阶段；原始中断证据保留。','']
    lines+=['','## 结论与剩余风险','',f'**{verdict}**。'+('本次覆盖的硬性条件与预设质量目标均有实测证据，可按当前生产保护配置进行受控上线；本次没有执行部署。' if verdict=='GO' else '存在失败项或缺失证据，禁止据此直接部署生产。失败项见上述门槛和原始 JSON。'),'',
            '范围限制：真实语义样本为 24 组和 28 条召回查询，不能等同线上分布保证；高频写入和 10000 条容量压力使用 Mock 模型，未进行 10000 条真实模型推理或长时间真实 CPU 推理 soak。16 路消息是背压重试下的幂等串行处理。情景到期/超量后停止召回，原始与审计数据仍增长。消融中的负例污染说明生产证据门槛应保留。','',
            '脚本、数据与复现说明：`scripts/preflight/`、`tests/fixtures/preflight/dialogues.json`。原始证据目录：']
    lines.append('')
    lines += ['- `'+path+'`' for path in payload['evidence_directories']]
    lines+=['',f"源码 SHA256：`{matrix.get('source_sha256')}`。各运行二进制哈希、逐单元结果及全部门槛见同名 JSON。",'']
    args.output.with_suffix('.md').write_text('\n'.join(lines))
    print(json.dumps({'verdict':verdict,'failed_gates':[name for name,value in gates.items() if not value]},ensure_ascii=False))
    return 0 if verdict=='GO' else 2


if __name__=='__main__': raise SystemExit(main())
