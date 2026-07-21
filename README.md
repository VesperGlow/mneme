# Qwen + SQLite 长期记忆助手

[![Build and test](https://github.com/VesperGlow/mneme/actions/workflows/build.yml/badge.svg)](https://github.com/VesperGlow/mneme/actions/workflows/build.yml)

这是一个可直接容器化部署的个人情感陪伴助手：**一个 Rust 单二进制**（axum + rusqlite + sqlite-vec + ort）内完成一切——进程内跑 uint8 量化的 `Qwen3-Embedding-0.6B` 与 `Qwen3-Reranker-0.6B`（ONNX）做向量化与二段精排，SQLite 单文件保存对话、长期记忆与情绪时间线；记忆**永久留存**，对话被压缩（旧消息滑出短期窗口）时便宜模型对整批对话做一次巩固——判断哪些值得记、对照已有记忆新增或更新，并抽取情绪，热路径每轮不再调记忆模型；主模型负责对话与工具调用；QQ 桥接按官方开放平台协议与私聊（C2C）通信。填好 `.env` 一条命令即可启动。

```mermaid
flowchart LR
    U[用户 / API] --> A[Memory Agent]
    A -->|当前消息| E[进程内 ONNX Embedding]
    E -->|1024 维向量| N[(SQLite)]
    A -->|压缩时批量巩固 + 情绪| S[便宜模型]
    A -->|对话 + 工具| L[主模型]
    S -->|长期记忆| A
    L -->|记忆工具 + 内置抓取 + MCP 外部工具| A
    F["内置 fetch_url"] -->|抓公开网页正文| L
    M["远程 MCP: Tavily"] <-->|联网搜索| L
    N -->|① 余弦召回候选| R[进程内 ONNX Reranker]
    R -->|② 精排 top-k| A
    Q[QQ 用户] <-->|WebSocket / HTTPS Webhook| B[QQ 桥接]
    B -->|内部 /v1/chat| A
    B -->|被动回复| Q
```

## 功能总览

- **长期记忆（压缩时巩固）**：不再每轮筛选用户单句，而是在对话被压缩时（旧消息滑出短期窗口）对整批已结束的对话做一次巩固——挑出值得长期记住的信息、对照相关已有记忆决定新增或更新（避免新旧矛盾并存），值得的就永久留存。上下文比逐条判断更完整，且热路径每轮零记忆模型调用。压缩只巩固「已滑出短期窗口」的部分，最近一小段仍在窗口内；另有**尾巴 flush**（`MEMORY_FLUSH_*`）定时扫描空闲够久的会话，把这段尾巴也强制巩固掉，避免用户长期沉默时最后说的话丢失。主模型仍可按需调用记忆工具（搜索 / 记住 / 遗忘 / 取代 / 关联）响应用户「记住这个 / 忘掉那个」这类主动指令。遗忘与取代都走软删除，保留可审计留痕。
- **短期上下文 + 滚动摘要**：每会话最近 N 条原文滑动窗口，确定顺序不丢；更早的消息后台压缩进会话摘要，超长对话也保留连续性。
- **单文件存储**：SQLite 一个文件装下对话、记忆向量（sqlite-vec/vec0）、实体关联与情绪时间线，备份即拷文件，所有记忆按 `user_id` 隔离。
- **两段式检索**：① sqlite-vec（vec0 虚拟表）做余弦 KNN 召回候选（个人库量级毫秒出结果），② 进程内 `Qwen3-Reranker-0.6B` 交叉编码器给 (query, 候选) 联合打分精排 top-k，精度远高于纯向量相似度；启用重排时召回段不做余弦地板预过滤，把完整候选池交给重排定夺（地板 `MEMORY_MIN_SCORE` 仅在回退到纯余弦时生效）；重排加载/推理失败会自动回退到 KNN 顺序、服务不受影响（见[数据结构](#数据结构)）。
- **记忆演变（SUPERSEDES）**：用户情况变化时新记忆取代旧记忆并保留可回溯的时间线。
- **记忆主体（subject）**：区分“关于用户”与“助手自己的承诺 / 人设”，检索时分组呈现、互不混淆。
- **情绪时间线**：从对话抽取情绪按时间成链，让助手感知跨会话情绪趋势。
- **分层提示词**：人设层（app 级 `PERSONA_PROMPT` 可整体替换口吻，对所有入口生效）与系统指令层（输出格式如禁用 Markdown、记忆/工具、安全，完整内容维护在 `.env.example` 的 `SYSTEM_INSTRUCTIONS` 里、非硬编码）分离，系统指令始终生效、优先于人设。
- **网页抓取（内置）**：内置 `fetch_url` 工具直接拉取公开链接、抽正文转 Markdown 回传给模型，纯 Rust、无外部服务；只处理静态/SSR 页面，不渲染 JS（见 [网页抓取与 MCP 工具](#网页抓取与-mcp-工具)）。
- **MCP 工具**：通过 `MCP_SERVERS_JSON` 接入 Tavily 联网搜索等远程 MCP 服务器（搜索需要索引，抓取工具替代不了，见 [网页抓取与 MCP 工具](#网页抓取与-mcp-工具)）。
- **纯私聊定位**：个人情感陪伴，只处理 QQ 私聊（C2C），不支持群聊与频道。
- **零本地依赖部署**：宿主机仅需 Docker，镜像由 GitHub Actions 编译并发布到 GHCR。

## 最快启动

宿主机只需要 Docker，不需要 Python、数据库或模型运行环境。

1. 安装 Docker Engine（Linux VPS）或 Docker Desktop（Windows/macOS），并确认 `docker compose version` 能运行。
2. 进入本目录，复制配置：

   ```sh
   cp .env.example .env
   ```

3. 编辑 `.env`，至少填写：

   ```dotenv
   AI_BASE_URL=https://你的供应商地址/v1
   AI_API_KEY=你的key
   MEMORY_MODEL=便宜模型名
   CHAT_MODEL=支持工具调用的主模型名
   APP_API_KEY=一段长随机字符串
   QQ_APP_ID=QQ开放平台的AppID
   QQ_APP_SECRET=QQ开放平台的AppSecret
   ```

4. 启动：

   ```sh
   docker compose up -d --build
   ```

5. 查看首次下载与启动进度：

   ```sh
   docker compose logs -f agent
   ```

embedding 模型首次启动会自动下载（约 640MB，缓存进 `model_cache` 卷，之后不再下）。完成后访问 `http://127.0.0.1:8000` 使用简易聊天页；API 文档在 `http://127.0.0.1:8000/docs`。

VPS 默认仅监听 `127.0.0.1`，建议用 SSH 隧道或反向代理加 HTTPS。确需对外提供应用 API 时，把 `APP_BIND_IP` 改为 `0.0.0.0`。

## 关键环境变量

| 变量 | 默认值 | 用途 |
|---|---|---|
| `AI_BASE_URL` | 无 | OpenAI-compatible API 根地址，代码会拼接 `/chat/completions`；也是记忆模型的默认接入点 |
| `AI_API_KEY` | 无 | AI 提供商密钥 |
| `MEMORY_MODEL` | 无 | 便宜的记忆筛选模型 |
| `CHAT_MODEL` | 无 | 对话和工具调用模型 |
| `MEMORY_BASE_URL` | 回退 `AI_BASE_URL` | 记忆模型的独立接入点；对话用 grok、记忆用 deepseek 这类跨供应商组合时填 deepseek 地址 |
| `MEMORY_API_KEY` | 回退 `AI_API_KEY` | 记忆模型接入点的密钥 |
| `MEMORY_EXTRA_HEADERS_JSON` | 回退 `AI_EXTRA_HEADERS_JSON` | 记忆模型专属额外请求头（JSON） |
| `CHAT_THINK` | `high` | 对话调用的思考深度：`off`/`low`/`medium`/`high`（别名 none/minimal/max 等自动归一）。需在 `AI_THINKING_MAP_JSON` 里配好该等级的片段才实际生效。记忆侧调用恒为 off |
| `AI_THINKING_MAP_JSON` | `{}` | 「思考等级 → 合并进请求体的字段片段」映射，各厂商差异只填这里，代码不变。片段里值为 `null` 的键会被删除（用于去掉推理模型不接受的字段）。示例见下 |
| `MEMORY_THINKING_MAP_JSON` | 回退 `AI_THINKING_MAP_JSON` | 记忆接入点的思考映射；跨供应商组合时单独填 |
| `PERSONA_PROMPT` | 无 | 全局人设（app 级），只写性格/口吻，对 QQ/网页/API 全部生效；留空用内置默认。输出格式、记忆工具与安全属独立的系统指令层，始终生效 |
| `SYSTEM_INSTRUCTIONS` | 见 `.env.example` | 系统指令层，完整推荐内容写在 `.env.example` 里（不硬编码在代码中）；留空时代码只兜底一句极简约束，不建议留空。多行用字面量 `\n`，自定义需自含格式与安全约束 |
| `MEMORY_CONSOLIDATE_ENABLED` | `true` | 自动记忆巩固：对话被压缩（旧消息滑出短期窗口）时，对整批对话做一次理解，喂入相关已有记忆做 reconcile、产出新增/更新并抽取情绪；热路径每轮不再调记忆模型 |
| `MEMORY_CONSOLIDATE_BATCH` | `6` | 累计这么多条「已滑出窗口且未巩固」的消息才触发一次巩固（与摘要各用独立水位线） |
| `MEMORY_FLUSH_ENABLED` | `true` | 尾巴 flush：定时扫描空闲会话，把最近一段仍在窗口内、平时不会 evict 的尾巴也巩固掉，避免用户长期沉默时尾巴丢失 |
| `MEMORY_FLUSH_IDLE_SECONDS` | `900` | 会话最后活动早于「现在 - 该秒数」才算空闲、可被 flush |
| `MEMORY_FLUSH_INTERVAL_SECONDS` | `300` | flush 扫描周期（秒） |
| `CONVERSATION_SUMMARY_ENABLED` | `true` | 滚动摘要：旧消息后台压缩进会话摘要 |
| `EMBEDDING_API_STYLE` | `local` | `local`（进程内 ONNX 推理）或 `openai`（远程接口，需配 `EMBEDDING_BASE_URL`） |
| `EMBEDDING_MODEL` | `electroglyph/Qwen3-Embedding-0.6B-onnx-uint8` | 本地 HF 仓库名或远程模型名 |
| `EMBEDDING_DIMENSIONS` | `1024` | 向量维度；Qwen 支持 Matryoshka 截取 32–1024 |
| `EMBEDDING_CONTEXT_SIZE` | `512` | 单条输入 token 上限，可设 `64..32768`；激活内存随长度增长（attention 部分平方级），越小推理峰值越低，超长输入自动截断。记忆/聊天场景 512 足够，做长文档向量化再调高 |
| `EMBEDDING_THREADS` | `4` | ONNX 推理线程数，按 CPU 核数调整 |
| `DB_PATH` | `/data/memory.db` | SQLite 数据库文件路径 |
| `RERANK_ENABLED` | `true` | 二段重排开关；关闭后检索退回纯余弦顺序 |
| `RERANK_MODEL` | `onnx-community/Qwen3-Reranker-0.6B-ONNX` | 重排模型仓库（onnx 可在 onnx/ 子目录）；加载失败自动回退纯余弦 |
| `RERANK_ONNX_FILE` | `quantized` | 仓库多个 onnx 变体里挑一个的偏好子串（`quantized`/`q4`/`fp16` …） |
| `RERANK_CANDIDATES` | `50` | 一段余弦召回、喂给重排器的候选宽度（再精排到 `MEMORY_SEARCH_LIMIT` 条） |
| `RERANK_CONTEXT_SIZE` | `512` | 重排单次 (query+候选) 的 token 上限 |
| `RERANK_THREADS` | `4` | 重排 ONNX 推理线程数 |
| `RERANK_INSTRUCTION` | 见默认 | 重排判定指令（Qwen3-Reranker 提示里的 Instruct 段） |
| `APP_API_KEY` | 无 | 此服务自己的 Bearer Token；公网部署必须配置 |
| `QQ_APP_ID` | 无 | QQ 开放平台机器人 AppID |
| `QQ_APP_SECRET` | 无 | QQ 机器人 AppSecret，用于 Access Token 和 Webhook 验签 |
| `QQ_EVENT_MODE` | `webhook` | QQ 事件接入模式：`websocket` 或 `webhook` |
| `QQ_WEBHOOK_PATH` | `/qqbot` | Webhook 模式下的 QQ 平台回调路径 |
| `FETCH_URL_ENABLED` | `true` | 是否启用内置 `fetch_url` 网页抓取工具 |
| `FETCH_TIMEOUT_SECONDS` | `30` | `fetch_url` 单次抓取超时（秒） |
| `FETCH_MAX_BYTES` | `5242880` | `fetch_url` 单页响应体下载上限（字节，流式截断） |
| `FETCH_RESULT_MAX_CHARS` | `12000` | `fetch_url` 回传给模型的正文字符上限 |
| `MCP_SERVERS_JSON` | `[]` | 远程 MCP 工具服务器列表，详见下方「网页抓取与 MCP 工具」 |
| `MCP_TIMEOUT_SECONDS` | `300` | 单次 MCP 工具调用的读超时（秒），超时会中断该次调用并把错误回传给模型 |
| `SHUTDOWN_TIMEOUT_SECONDS` | `30` | 优雅停机：收到 SIGTERM/Ctrl-C 后等待在途消息与落库（历史/记忆/摘要/情绪）完成的上限；容器 stop 宽限期需大于该值（compose 已设 40s） |
| `CHAT_IMAGE_ENABLED` | `true` | 图片理解：QQ 图片附件 / API `images` 以 image_url 传给 `CHAT_MODEL`（须支持视觉）。图片不落库，历史记为 `[图片×N]` |
| `CHAT_IMAGE_MAX_COUNT` | `3` | 单条消息最多带几张图片 |
| `CHAT_IMAGE_MAX_BYTES` | `5242880` | 发送给模型的单张上限；超限的图自动压缩（缩放 + 重编码 JPEG） |
| `CHAT_IMAGE_FETCH_MAX_BYTES` | `31457280` | 从 QQ CDN 下载原图的上限（压缩前） |
| `CHAT_IMAGE_MAX_EDGE` | `2048` | 图片长边像素上限，超过则等比缩放 |
| `LOG_CONTENT_PREVIEW_CHARS` | `40` | 运行日志里消息/记忆内容预览的最大字符数；`0` 完全不记内容，日志只留长度与统计 |

> 机器人定位为个人情感陪伴，仅处理 QQ 私聊（C2C），不支持群聊与频道。

### 思考深度（reasoning / thinking）

代码里只有 `off`/`low`/`medium`/`high` 四档抽象等级（`CHAT_THINK`）。各家厂商用什么字段表达思考深度，全部写在 `AI_THINKING_MAP_JSON` 里，换厂商只改这份 env、不改代码。请求前会把当前等级对应的片段深合并进请求体；片段里值为 `null` 的键会被删除。截至 2026-07 各家兼容端点的写法：

```sh
# xAI grok-4.5：默认 high、不可关闭；开启推理时禁止 stop（用 null 删掉）
AI_THINKING_MAP_JSON='{"low":{"reasoning_effort":"low"},"high":{"reasoning_effort":"high","stop":null}}'

# OpenAI gpt-5.x：reasoning_effort 直传；推理模型不接受 temperature，置 null 删除
AI_THINKING_MAP_JSON='{"low":{"reasoning_effort":"low","temperature":null},"high":{"reasoning_effort":"high","temperature":null}}'

# DeepSeek V4：reasoning_effort 只吃 high/max，且需伴随 thinking.type=enabled
AI_THINKING_MAP_JSON='{"off":{"thinking":{"type":"disabled"}},"high":{"reasoning_effort":"high","thinking":{"type":"enabled"}}}'

# Qwen3.x：非标字段 enable_thinking（本项目直拼裸 JSON，直接顶层放即可，无需 extra_body）
AI_THINKING_MAP_JSON='{"off":{"enable_thinking":false},"high":{"enable_thinking":true}}'

# Gemini 3 兼容层：低/高映射到思考预算；medium 部分模型会 400，谨慎用
AI_THINKING_MAP_JSON='{"low":{"reasoning_effort":"low"},"high":{"reasoning_effort":"high"}}'
```

某个等级在映射里没有对应片段时，请求体就不带任何思考字段，由厂商用自己的默认值决定。`DeepSeek-reasoner` 那种「靠换模型名开思考、无参数」的旧法不属于这一维，用 `MEMORY_MODEL`/`CHAT_MODEL` 直接选型即可。`GET /v1/config` 会回显当前 `chat_think` 与已配置的等级列表。

向量存在 sqlite-vec 的 vec0 表里，维度在建表时固定。要改变 `EMBEDDING_DIMENSIONS` 或换 embedding 模型，需要重建 `vec_memories` 表并重新生成全部向量；全新测试环境也可以用 `docker compose down -v` 清空数据后重建（这会永久删除全部记忆数据和模型缓存）。

### 查看已存记忆（CLI 子命令）

二进制（`mneme`）带参数即当作一次性子命令，直接查/改库并退出，不启动服务。运维排查时无需 sqlite、也不用管卷路径，`exec` 进容器即可：

```sh
podman exec <容器> mneme memory list                 # 活跃记忆（默认最多 200 条）
podman exec <容器> mneme memory list --user qq:c2c:xxxx
podman exec <容器> mneme memory list --limit 50 --json
podman exec <容器> mneme memory delete <id> <id> ... # 硬删除一条或多条（不可逆）
podman exec <容器> mneme memory delete --all --yes   # 硬删除全部记忆（不可逆）
podman exec <容器> mneme memory stats                # 按活跃/类型汇总条数，删完核对
```

> 容器名 `<容器>` 取决于你的 quadlet `ContainerName`（默认部署里是 `mneme`，即 `podman exec mneme mneme memory list`）。

三个子命令：`list` 看、`stats` 数、`delete` 删。`list`/`stats` 只读打开（`query_only`），只查 `memories` 等普通表、不碰 vec0，与运行中的服务共享同一 WAL 库、互不影响；`list` 只列活跃记忆，默认文本表格每行为 `时间 kind ×重复次数 id前缀 摘要`（单用户时用户列省略、只在标题带一次 `user=`，多用户才逐行加 `[尾号]`），`--json` 输出要点字段。`delete` 是写操作、**硬删除**：彻底 `DELETE` 记忆行 + FK 级联清实体链接 + 移出 vec0 向量索引，`memories` 与 vec0 放同一事务（WAL + busy_timeout 与服务并发安全），**不可逆**；认 id 前缀（像 git 短哈希，如 `memory delete 7642e9b1`），可一次给多个 id，`--all` 删全部、必须配 `--yes` 确认（moods 情绪时间线不受影响）。`stats` 是只读汇总（活跃/失效计数、按类型分布、时间跨度），只给数不给内容。

## 对话 API

```sh
curl http://127.0.0.1:8000/v1/chat \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer 你的APP_API_KEY' \
  -d '{
    "user_id": "sorak",
    "message": "请记住，我偏好简洁的中文回答。"
  }'
```

可选 `images` 字段附带图片（需 `CHAT_MODEL` 支持视觉）：元素可以是裸 base64、`data:image/…;base64,…` data URI，或 http(s) 图片 URL；带图片时 `message` 可以为空。数量与大小上限见 `CHAT_IMAGE_MAX_COUNT` / `CHAT_IMAGE_MAX_BYTES`。QQ 私聊里直接发图即可，图文混发会一起理解。

响应会包含：

- `message`：主模型回答；
- `retrieved_memories`：本轮向量检索命中的记忆；
- `saved_memories`：本轮由主模型记忆工具即时保存的记忆（自动记忆已改到压缩时后台巩固，不在本轮返回，故通常为空）；
- `tool_events`：主模型调用过的记忆工具；
- `conversation_id`：后续请求带回即可保留短期对话历史。

主要接口：

- `POST /v1/chat`：对话；
- `POST /v1/memories`：手工写入记忆；
- `GET /v1/memories/search`：语义搜索；
- `GET /v1/memories/recent`：最近记忆；
- `DELETE /v1/memories/{id}`：软删除/遗忘；
- `GET /v1/memories/{id}/history`：沿 SUPERSEDES 链回溯一条记忆的演变时间线；
- `POST /v1/memories/link`：建立记忆关系；
- `GET /v1/mood/{user_id}`：情绪时间线与近期趋势聚合；
- `GET /v1/graph/{user_id}`：导出小型图谱快照；
- `GET /health`：检查三项依赖。

## 网页抓取与 MCP 工具

### 内置 fetch_url（网页抓取）

主模型内置一个 `fetch_url` 工具：给它一个公开 http/https 链接，它会拉取页面、用 readability 抽取正文、转成 Markdown 回传给模型。纯 Rust 实现（`dom_smoothie` + `htmd`），进程内完成，不依赖任何外部服务或浏览器。

适用与边界：

- **能**：静态或服务端渲染的页面——新闻、博客、文档站、维基、GitHub 页面等，覆盖"发个链接看看内容"的绝大多数场景。
- **不能**：不执行页面 JS，所以纯前端渲染的 SPA、以及被 Cloudflare「Just a moment...」这类 JS 验证墙挡住的站点抓不到正文；也不能替代**搜索**——搜索依赖索引，不是抓取能替代的（这也是为什么保留 Tavily）。
- **安全**：内置 SSRF 防护，只放行 http/https，且解析出的目标地址必须是公网地址，拒绝 localhost、内网、链路本地与云元数据地址（`169.254.169.254` 等）；逐跳校验重定向，响应体按 `FETCH_MAX_BYTES` 流式截断。

用 `FETCH_URL_ENABLED=false` 可整体关闭。

### MCP 工具（远程，联网搜索等）

主模型除了内置的记忆与抓取工具，还可以调用远程 MCP 服务器提供的工具。通过 `MCP_SERVERS_JSON`（JSON 数组）配置，每项字段：

- `name`（必填）：服务器标识，工具会以 `mcp__<name>__<tool>` 暴露给模型；
- `url`（必填）：MCP 服务器地址，可用 `${NAME}` 引用环境变量（便于只在 env 填 key）；
- `transport`：`streamable_http`（默认）或 `sse`；
- `headers`：可选请求头对象，同样支持 `${NAME}`；
- `tools` / `exclude`：工具名白名单 / 黑名单，按需挑选以节省 token；
- `enabled`：设为 `false` 可临时停用某项。

Tavily 提供托管的 streamable-http 端点，把 API key 单独放进环境变量、URL 里用 `${...}` 引用即可。网页抓取已由内置 `fetch_url` 承担，这里默认只注册 Tavily 联网搜索：

```dotenv
TAVILY_KEY=tvly-你的KEY
MCP_SERVERS_JSON=[{"name":"tavily","url":"https://mcp.tavily.com/mcp/?tavilyApiKey=${TAVILY_KEY}","tools":["tavily_search"]}]
```

`CHAT_MODEL` 需支持 OpenAI tool calling；`GET /health` 的 `mcp_tools` 字段会显示已注册的 MCP 工具数量。

## 数据结构

所有数据在一个 SQLite 文件里（`DB_PATH`，默认 `/data/memory.db`，WAL 模式）：

- `conversations` / `messages`：短期对话历史（`seq` 自增保证顺序确定；会话上的 `summary` 滚动压缩更早的对话）；
- `memories`：长期记忆——正文、`subject`（user/assistant）等元数据，永久留存；软删除/取代都保留可审计留痕；
- `vec_memories`：sqlite-vec 的 vec0 虚拟表，存记忆向量（float32、余弦距离），rowid 对应 `memories` 行、只含活跃记忆，供 KNN 召回；
- `entities` / `memory_entities`：记忆提及的人、项目、地点等实体关联；
- `memory_links`：主模型建立的记忆间关系；
- 记忆演变：`superseded_by` 链记录取代关系，旧记忆软停用但保留，可经 `/history` 回溯时间线；
- `moods`：情绪时间线，从每条消息抽取的情绪（label/valence/note）按时间排列。

情绪识别折叠进"记忆巩固"那一次廉价模型调用里（不额外耗 token），压缩时从整批对话中提炼明显流露的情绪。每轮对话前会把近期情绪趋势压成一行注入上下文，让助手自然体察用户状态。由 `MOOD_TRACKING_ENABLED` 开关、`MOOD_TREND_DAYS` 控制回看窗口。

所有记忆操作都按 `user_id` 隔离。遗忘采用软删除，节点仍可审计但不会再被检索。

每条记忆带 `subject` 区分主体：`user`（关于用户的事实/偏好，自动筛选只产出这类）与 `assistant`（助手自己对用户的承诺、约定或人设设定）。检索时按主体分组呈现给模型、互不混淆；写入会按主体隔离去重；旧数据无该字段时默认视为 `user`。

检索分两段：① 在 vec0 里对该用户的记忆向量做余弦 KNN（向量入库前已归一化；个人记忆库量级毫秒出结果），取 top-`RERANK_CANDIDATES` 候选；② 用进程内 `Qwen3-Reranker-0.6B` 交叉编码器给每个 (query, 候选正文) 联合打分，精排取 top-`MEMORY_SEARCH_LIMIT`。启用重排时第 ① 段**不做余弦地板（`MEMORY_MIN_SCORE`）预过滤**——把完整候选池交给重排定夺相关性，避免向量相似度偏低但实际相关的记忆在进重排前就被砍掉；`MEMORY_MIN_SCORE` 只在「余弦是最终排序信号」时生效，即重排关闭、模型加载失败或推理出错、回退到第 ① 段余弦顺序的场景。回退时检索照常返回。返回给上层的 `score`：重排生效时是重排相关性概率，回退时是原始余弦相似度。

## 资源建议

- CPU 部署：2 核、2 GB RAM 起步。embedding 与 reranker 两个 uint8 量化模型各约 650MB，合计常驻约 1.3GB，稳态总占用往 1.5GB 走，compose 默认 `mem_limit: 2g` 兜底偏紧；内存紧张时可 `RERANK_ENABLED=false` 关掉重排（省下一个模型、退回纯余弦），或调低 `EMBEDDING_CONTEXT_SIZE` / `RERANK_CONTEXT_SIZE`。
- 磁盘：建议预留 2–3 GB 给镜像、模型缓存和数据库。
- **怎么看内存水位**：模型权重是 mmap 的文件页缓存，内存紧张时内核可直接回收，不是硬占用——所以 `docker stats` / RSS 显示的数字会明显偏大。判断真实水位看匿名内存：`cat /proc/<pid>/smaps_rollup` 里的 `Anonymous` 行（容器内 pid 1），或 cgroup 的 `memory.stat` 里 `anon` 项。给 `mem_limit` 定值时按匿名内存 + 少量余量即可，不必为页缓存留满 650MB。
- 不想在本机跑推理时，设 `EMBEDDING_API_STYLE=openai` 用供应商的 embedding 接口（注意换模型/维度需重建已有记忆向量）。

## 备份与更新

SQLite 数据库和模型缓存分别保存在 Docker volume `app_data`、`model_cache`。备份只需拷出 `/data/memory.db` 一个文件。不要把 `docker compose down -v` 当成普通停止命令；日常停止使用：

```sh
docker compose stop
```

> **首次启动会做一次向量迁移**：服务启动时把记忆向量整理进 `vec_memories`（vec0）表；从更早的版本（向量曾存在 `memories.embedding` f16 BLOB 列）升级时，首启会回填 vec0 后**移除该列，不可逆**。所以升级前先备份：`cp memory.db memory.db.bak`。首启日志出现 `向量已迁移到 vec0：N 条` 即迁移成功（全新库无此列，直接跳过）。

查看错误：

```sh
docker compose ps
docker compose logs --tail=200 agent
```

如果主模型供应商不接受 `tools` 参数，服务会保留自动向量检索并降级为普通对话，同时在 `warnings` 中说明；要完整使用“记住/遗忘/关联”工具，应选择支持 OpenAI tool calling 格式的模型。

## 接入 QQ 机器人

QQ 桥接按腾讯官方开放平台协议实现（与 `tencent-connect/botgo` 行为对齐），自动用 `AppID + AppSecret` 获取并刷新 Access Token，并可通过 `QQ_EVENT_MODE` 在 WebSocket 与 HTTPS Webhook 之间切换。

1. 在 [QQ 开放平台](https://q.qq.com/) 创建机器人，把 `AppID` 和 `AppSecret` 写入 `.env`。
2. 使用 WebSocket 时设置以下变量。它由容器主动连接 QQ，不需要公网域名或反向代理：

   ```dotenv
   QQ_EVENT_MODE=websocket
   ```

3. 使用 Webhook 时设置 `QQ_EVENT_MODE=webhook`，并给容器的 `9000` 端口配置公网 HTTPS 反向代理。默认宿主机只监听 `127.0.0.1:9000`，例如 Nginx：

   ```nginx
   location /qqbot {
       proxy_pass http://127.0.0.1:9000;
       proxy_set_header Host $host;
       proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
       proxy_set_header X-Forwarded-Proto https;
   }
   ```

4. Webhook 模式下，在 QQ 开放平台把回调地址配置为 `https://你的域名/qqbot`。平台会发起签名校验，服务会自动完成响应。
5. 本项目仅订阅私聊事件 `C2C_MESSAGE_CREATE`（个人情感陪伴定位，不处理群聊与频道）。

6. 把 VPS 的固定公网出口 IP 加入机器人 IP 白名单；机器人上线前，在开放平台配置沙箱成员。
7. 检查桥接状态和日志：

   ```sh
   curl http://127.0.0.1:9000/healthz
   docker compose logs -f agent
   ```

Webhook 收到事件后会立即确认，再异步调用 AI，避免慢模型触发平台重试；WebSocket 会自动维护会话、心跳和重连。两种模式共用同一套消息处理逻辑：按 `msg_id` 去重、按用户会话串行处理，并用 `msg_seq` 对长回复分片。QQ 的 OpenID 只以稳定哈希形式写入数据库，不直接保存原始 OpenID。

目前 QQ 附件只会得到“暂不支持”的文字提示；文本消息、记忆检索、自动记忆和主模型工具调用均完整接通。

## GHCR 镜像

`main` 分支通过测试后，GitHub Actions 会发布一个 `linux/amd64` 镜像（单个 Rust 二进制，包含 API、embedding、存储与 QQ 桥接）：

```text
ghcr.io/vesperglow/mneme:latest
```

每次发布也会生成 `sha-<完整提交号>` 标签，生产环境可以锁定该标签，避免 `latest` 变化。

在 VPS 上使用预构建镜像：

```sh
cp .env.example .env
# 编辑 .env 后：
docker compose pull
docker compose up -d --no-build
```

GHCR 首次发布的个人包通常是私有的。私有状态下，先创建带 `read:packages` 权限的 GitHub PAT，然后登录：

```sh
echo "$GHCR_TOKEN" | docker login ghcr.io -u VesperGlow --password-stdin
```

如需免登录拉取，请进入 [mneme 包设置](https://github.com/users/VesperGlow/packages/container/mneme/settings)，将可见性改为 `Public`。

本地开发仍可使用 `docker compose up -d --build`，Compose 会按 `APP_IMAGE` 给本地构建结果打标签。
