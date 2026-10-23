# Mneme 本地上线前验收

这些脚本只使用临时 SQLite、独立端口、带所有权标记的 Neo4j 和合成数据。不读取 `.env`，不操作生产服务。所有压力流量使用 Mock LLM；真实压缩和真实模型召回另行运行。`preflight_server` 拒绝临时目录以外的配置/数据库及非回环上游。

## 环境与运行

需要 Rust、Python 3.12+、Linux，建议 4 CPU、8 GB 以上内存与 8 GB 空闲磁盘。下载约 1.8 GB 的仓库固定模型权重，另有 CPU PyTorch、JRE 和 Neo4j。模型文件按仓库清单校验，独立运行 Neo4j 5.26.32 / JRE 21；没有 Docker 依赖。

```bash
python3 scripts/preflight/services.py --runtime /tmp/mneme-preflight-qa --download --start
```

默认采用部署配置的 FP32 Embedding / 3 线程；BF16 / 2 线程对照可加 `--embedding-dtype bfloat16 --cpu-threads 2`。该命令以前台模式保持测试服务；Ctrl-C 仅关闭本次创建的进程。端口 18474 / 19090 已占用时直接拒绝启动。已存在且无所有权标记的目录也会被拒绝，不能指向生产目录。

另一个终端中构建本机验收服务并运行压力矩阵：

```bash
cargo build --example preflight_server
/tmp/mneme-preflight-qa/venv/bin/python scripts/preflight/run.py \
  --graph-owner-file /tmp/mneme-preflight-qa/graph-owner.json \
  --output artifacts/preflight-matrix \
  --phases long,concurrent,faults,quality,capacity,retrieval
```

输出目录必须不存在。每次运行固定服务二进制、保存源码与二进制哈希，防止测试过程中重新编译造成不同阶段混用版本。原始结果、逐请求压缩来源、RSS/CPU/FD/I/O/队列样本及 SQLite 数据均保留在输出目录。`results.json` 在每个单元结束时刷新。

- `long`：不少于 1 万 / 5 万 / 10 万 o200k Token，真实 HTTP 聊天、上下文压缩、SQLite 原文及来源回执核对。按完整轮次重算 2048 阈值；最后把本次拥有的测试 DB 中未处理轮次 `completed_at` 推前 121 秒，加速验证 120 秒排队机制。
- `concurrent`：1 / 4 / 16 并发，各 64 条消息，同时检索，再重放已完成 request ID。409 / 429 作为明确背压记录并计入完成延迟，重试仍使用原 request ID。
- `faults`：Embedding 超时/503、Neo4j 503、真实 TCP keep-alive 连接复位与拒绝连接、无效 Reranker、无效压缩 JSON，以及 SIGKILL 本次 Mneme 服务后从相同 DB 恢复。TCP 桥只操作自己的连接，不停止 Neo4j。
- `quality`：120 组、12 类带标签合成对话。Mock 只按原句回放，结果只能证明存储契约，不能作为模型语义准确率。
- `capacity`：1000 / 10000 条长期记忆，真实 SQLite / Neo4j，预计算确定性向量，以隔离存储与索引开销；每级 24 次混合检索。它不是 10000 条真实 Embedding 的推理吞吐测试。Neo4j 是固定大小分配的存储；比较首次空间增长应使用新的自有数据目录，不能把复用数据库的文件高水位当成新增占用。
- `retrieval`：真实 Neo4j + 确定性词法模型，用于验证评估链路，不能声称真实语义质量。

## 少量真实模型验收

创建权限为 0600 的凭证文件，放在仓库外。仅支持本次授权的 DeepSeek 官方接口，其他地址或模型会被拒绝：

```json
{"base_url":"https://api.deepseek.com/v1","model":"deepseek-flash","key":"YOUR_TEST_KEY"}
```

```bash
/tmp/mneme-preflight-qa/venv/bin/python scripts/preflight/run.py \
  --graph-owner-file /tmp/mneme-preflight-qa/graph-owner.json \
  --output artifacts/preflight-live \
  --provider-file /tmp/preflight-provider.json \
  --phases quality_live,retrieval_real,embedding_speed
```

`quality_live` 对 24 组分层抽样对话使用真实 deepseek-flash 压缩、生产语义协调、真实 Embedding/Reranker 和 Neo4j。聊天生成仍为 Mock，避免无意义的付费生成。中继关闭 thinking、限制最多 48 个真实请求和 50 万输入+输出 Token；未知用量按预留上限记账。凭证不进入服务配置、请求证据、日志或报告。

`retrieval_real` 使用仓库固定的 Qwen3-Embedding-0.6B（BF16）、gte-multilingual-reranker-base（FP32）、2 个 CPU 推理线程及真实 Neo4j。123 条文档（原有 23 条 + 100 条干扰）、28 条标准答案查询。单独测向量、全文、向量/全文 RRF、RRF+Reranker，并额外测实际生产混合检索/精排。第一阶段使用独立查询，不能把精排超时导致调试轨迹缺失误报为 ANN 漏检。

`capacity_reranker` 在相同的 1000/10000 条预计算向量容量集上，改用真实本地 GTE Reranker 执行每级 24 次检索，用于核实 Mock 词法分数饱和与近重复编号辨识；它仍不声称完成 10000 条真实文档推理。

`embedding_speed` 对 32 个全新文本测真实冷推理，再对相同文本测热缓存，记录模型返回的实际 Token 及缓存 Token。两者不能混报。正式性能采样应避免同机编译或其他压测竞争 CPU。

`acceptance.json` 是本次实测前确定的硬性条件及质量目标；`Precision@5` 固定除以 5，同时给出已返回结果的准确率。正例 Recall/MRR 与负例污染率分开计算。向量/全文候选阶段不承担最终负例拒绝职责，实际生产最终结果必须符合严格门槛。

## 回归

```bash
cargo test -- --test-threads=4
cargo clippy --all-targets -- -D warnings
HF_HOME=/tmp/mneme-preflight-qa/hf HF_HUB_OFFLINE=1 \
  /tmp/mneme-preflight-qa/venv/bin/python -m unittest discover -s tests -p 'test_*.py'
```

报告必须标明 Mock / 真实模型边界、缓存状态、测试硬件、未覆盖范围及 GO / NO-GO。没有完整原文、任务恢复和真实质量证据时不能批准上线；这些脚本不包含生产部署命令。

## 生成验收报告

```bash
python3 scripts/preflight/report.py \
  --matrix artifacts/preflight-matrix \
  --live artifacts/preflight-live \
  --embedding artifacts/preflight-live \
  --default-profile artifacts/preflight-default \
  --output docs/reports/memory-preflight-YYYY-MM-DD
```

实际使用的完整证据命令及目录保存在本次报告中。`--continuation` 允许在同一应用源码版本下续跑中断阶段，原始中断原因会保留；`--capacity-retest` 单独记录真实 Reranker 容量复测，Mock 失败数据仍保留。报告会拒绝不同应用源码混用；缺少必要实测时返回 NO-GO 和非零退出码，不会自动部署。
