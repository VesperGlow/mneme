# DeepSeek + SQLite 长期记忆助手

[![Build and test](https://github.com/VesperGlow/mneme/actions/workflows/build.yml/badge.svg)](https://github.com/VesperGlow/mneme/actions/workflows/build.yml)

这是一个可直接容器化部署的个人情感陪伴助手：**一个 Rust 单二进制**（axum + rusqlite）内完成一切——SQLite 单文件保存对话、长期记忆与情绪时间线；记忆**永久留存**，对话被压缩（旧消息滑出短期窗口）时便宜模型对整批对话做一次巩固——判断哪些值得记、对照已有记忆新增或更新，并抽取情绪，热路径每轮不再调记忆模型；检索也交给同一个便宜模型：候选池整池喂过去让它挑，**没有 embedding、没有重排器、没有任何本地推理**；主模型负责对话与工具调用；QQ 桥接按官方开放平台协议与私聊（C2C）通信。填好 `.env` 一条命令即可启动。

模型侧只对接 **DeepSeek 官方 API**，且不做成可配置项（KISS）：对话用 `deepseek-v4-pro`、**思考等级固定 `max`**，记忆（精选 / 巩固 / 摘要）用 `deepseek-v4-flash` 并关闭思考。环境变量只剩部署真正必需的那几个，其余参数全是 `src/config.rs` 顶部的常量。

```mermaid
flowchart LR
    U[用户 / API] --> A[Memory Agent]
    A -->|当前消息| N[(SQLite)]
    N -->|候选池: 按新近度取活跃记忆| S
    A -->|压缩时批量巩固 + 情绪| S[便宜模型]
    A -->|对话 + 工具| L[主模型]
    S -->|长期记忆 + 精选出的相关记忆| A
    L -->|记忆工具 + 内置抓取 + MCP 外部工具| A
    F["内置 fetch_url"] -->|抓公开网页正文| L
    M["远程 MCP: Tavily"] <-->|联网搜索| L
    Q[QQ 用户] <-->|WebSocket / HTTPS Webhook| B[QQ 桥接]
    B -->|内部 /v1/chat| A
    B -->|被动回复| Q
```

## 功能总览

- **长期记忆（压缩时巩固）**：不再每轮筛选用户单句，而是在对话被压缩时（旧消息滑出短期窗口）对整批已结束的对话做一次巩固——挑出值得长期记住的信息、对照相关已有记忆决定新增或更新（避免新旧矛盾并存），值得的就永久留存。上下文比逐条判断更完整，且热路径每轮零记忆模型调用。压缩只巩固「已滑出短期窗口」的部分，最近一小段仍在窗口内；另有**尾巴 flush**（`MEMORY_FLUSH_*`）定时扫描空闲够久的会话，把这段尾巴也强制巩固掉，避免用户长期沉默时最后说的话丢失。主模型仍可按需调用记忆工具（搜索 / 记住 / 遗忘 / 取代 / 关联）响应用户「记住这个 / 忘掉那个」这类主动指令。遗忘与取代都走软删除，保留可审计留痕。
- **短期上下文 + 滚动摘要**：每会话最近 N 条原文滑动窗口，确定顺序不丢；更早的消息后台压缩进会话摘要，超长对话也保留连续性。
- **单文件存储**：SQLite 一个文件装下对话、记忆、实体关联与情绪时间线，全是普通关系表，备份即拷文件，所有记忆按 `user_id` 隔离。
- **记忆精选检索（无模型）**：不做向量匹配也不做关键词匹配——把该用户按新近度取的候选池（最多 `MEMORY_SELECT_POOL_MAX` 条活跃记忆）整池交给便宜的记忆模型，让它挑出对本轮回复真正有用的几条。语义理解、指代消解（「上次说的那家店」）、否定判别都由它一并完成，这些正是关键词检索在中文闲聊里做不到的地方。候选池不超过 `MEMORY_SEARCH_LIMIT` 时直接全返、不调模型；模型调用失败或返回不可解析时回退到「最近 N 条」，**永不因检索失败中断对话**（见[数据结构](#数据结构)）。
- **记忆演变（SUPERSEDES）**：用户情况变化时新记忆取代旧记忆并保留可回溯的时间线。
- **记忆主体（subject）**：区分“关于用户”与“助手自己的承诺 / 人设”，检索时分组呈现、互不混淆。
- **情绪时间线**：从对话抽取情绪按时间成链，让助手感知跨会话情绪趋势。
- **分层提示词**：人设层（app 级 `PERSONA_PROMPT` 可整体替换口吻，对所有入口生效）与系统指令层（输出格式如禁用 Markdown、记忆/工具、安全；完整默认内容在 `src/agent.rs` 的 `DEFAULT_SYSTEM_INSTRUCTIONS`，可用 `SYSTEM_INSTRUCTIONS` 整体替换）分离，系统指令始终生效、优先于人设。
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
   DEEPSEEK_API_KEY=sk-你的KEY
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

没有模型要下载，起来就能用：访问 `http://127.0.0.1:8000` 使用简易聊天页；API 文档在 `http://127.0.0.1:8000/docs`。

VPS 默认仅监听 `127.0.0.1`，建议用 SSH 隧道或反向代理加 HTTPS。确需对外提供应用 API 时，把 `APP_BIND_IP` 改为 `0.0.0.0`。

## 环境变量

全部就这些，没有别的：

| 变量 | 默认值 | 用途 |
|---|---|---|
| `DEEPSEEK_API_KEY` | 无 | DeepSeek 官方 API 密钥，对话与记忆共用 |
| `APP_API_KEY` | 无 | 此服务自己的 Bearer Token；留空不鉴权，公网部署必须配置 |
| `PERSONA_PROMPT` | 无 | 全局人设（app 级），只写性格/口吻，对 QQ/网页/API 全部生效；留空用内置默认。输出格式、记忆工具与安全属独立的系统指令层，始终生效 |
| `SYSTEM_INSTRUCTIONS` | 无 | 系统指令层（输出格式/记忆工具/安全），优先于人设。留空即用代码里的完整默认内容（`src/agent.rs` 的 `DEFAULT_SYSTEM_INSTRUCTIONS`），一般不必填；要整体替换时多行用字面量 `\n`，且需自含格式与安全约束 |
| `LOG_LEVEL` | `INFO` | 日志级别 |
| `DB_PATH` | `/data/memory.db` | SQLite 数据库文件路径 |
| `MCP_SERVERS_JSON` | `[]` | 远程 MCP 工具服务器列表，详见下方「网页抓取与 MCP 工具」 |
| `QQ_APP_ID` | 无 | QQ 开放平台机器人 AppID |
| `QQ_APP_SECRET` | 无 | QQ 机器人 AppSecret，用于 Access Token 和 Webhook 验签 |
| `QQ_EVENT_MODE` | `webhook` | QQ 事件接入模式：`websocket` 或 `webhook` |

> 机器人定位为个人情感陪伴，仅处理 QQ 私聊（C2C），不支持群聊与频道。

### 其余参数（代码常量）

模型名、思考等级、超时、记忆策略、并发与限流全部固定在 `src/config.rs` 顶部，改它们要重新构建镜像。这是刻意的：这些值只有开发时才该动，暴露成 env 只会让部署面变大。常用的几个：

| 常量 | 值 | 说明 |
|---|---|---|
| `CHAT_MODEL` | `deepseek-v4-pro` | 对话与工具调用 |
| `MEMORY_MODEL` | `deepseek-v4-flash` | 记忆精选 / 巩固 / 摘要 |
| `REASONING_EFFORT` | `max` | 对话侧固定最高思考等级 |
| `CHAT_MAX_OUTPUT_TOKENS` | `8192` | 思考模式下 CoT 也算输出，留足额度 |
| `MEMORY_SEARCH_LIMIT` | `8` | 单轮最多注入上下文的记忆条数 |
| `MEMORY_SELECT_POOL_MAX` | `400` | 精选候选池上限（按 `last_seen_at` 倒序） |
| `MEMORY_DUPLICATE_THRESHOLD` | `0.9` | 近似去重阈值（字符三元组 Jaccard） |
| `MEMORY_CONSOLIDATE_BATCH` | `6` | 攒够多少条已滑出窗口的消息才巩固一次 |
| `MEMORY_FLUSH_IDLE_SECONDS` | `900` | 会话空闲多久后强制巩固尾巴 |
| `SHUTDOWN_TIMEOUT_SECONDS` | `30` | 优雅停机等待在途写入的上限（compose 的 stop 宽限期已设 40s） |
| `LOG_PREVIEW_CHARS` | `40` | 日志里内容预览的字符数；`0` 则完全不记内容 |

### 思考深度（reasoning / thinking）

不可配置，**恒为最高档**：对话请求固定带 `thinking: {"type":"enabled"}` + `reasoning_effort: "max"`（DeepSeek 只有 `high` / `max` 两档）。思考模式不接受 `temperature` / `top_p` / 惩罚项，所以对话请求不发温度参数。

记忆侧（精选、巩固、摘要）反过来固定 `thinking: {"type":"disabled"}`：这些是结构化短任务，要的是快和便宜，思考只会拖慢并烧钱。`GET /v1/config` 会回显当前模型与思考等级。

检索没有需要重建的索引：候选池是直接从 `memories` 表按 `last_seen_at` 查出来的，调 `MEMORY_SELECT_POOL_MAX` 重新构建即生效，不涉及任何数据重算。

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

三个子命令：`list` 看、`stats` 数、`delete` 删。`list`/`stats` 只读打开（`query_only`），与运行中的服务共享同一 WAL 库、互不影响；`list` 只列活跃记忆，默认文本表格每行为 `时间 kind ×重复次数 id前缀 摘要`（单用户时用户列省略、只在标题带一次 `user=`，多用户才逐行加 `[尾号]`），`--json` 输出要点字段。`delete` 是写操作、**硬删除**：彻底 `DELETE` 记忆行 + FK 级联清实体链接（WAL + busy_timeout 与服务并发安全），**不可逆**；认 id 前缀（像 git 短哈希，如 `memory delete 7642e9b1`），可一次给多个 id，`--all` 删全部、必须配 `--yes` 确认（moods 情绪时间线不受影响）。`stats` 是只读汇总（活跃/失效计数、按类型分布、时间跨度），只给数不给内容。

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

只吃纯文本：DeepSeek 没有视觉接口，图片理解已整体移除（QQ 收到图片附件会礼貌说明看不了）。

响应会包含：

- `message`：主模型回答；
- `retrieved_memories`：本轮精选出的相关记忆；
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
- **安全**：内置 SSRF 防护，只放行 http/https，且解析出的目标地址必须是公网地址，拒绝 localhost、内网、链路本地与云元数据地址（`169.254.169.254` 等）；逐跳校验重定向，响应体按 `FETCH_MAX_BYTES`（代码常量，5 MiB）流式截断。

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

`GET /health` 的 `mcp_tools` 字段会显示已注册的 MCP 工具数量。

## 数据结构

所有数据在一个 SQLite 文件里（`DB_PATH`，默认 `/data/memory.db`，WAL 模式）：

- `conversations` / `messages`：短期对话历史（`seq` 自增保证顺序确定；会话上的 `summary` 滚动压缩更早的对话）；
- `memories`：长期记忆——正文、`subject`（user/assistant）等元数据，永久留存；软删除/取代都保留可审计留痕；
- `entities` / `memory_entities`：记忆提及的人、项目、地点等实体关联；
- `memory_links`：主模型建立的记忆间关系；
- 记忆演变：`superseded_by` 链记录取代关系，旧记忆软停用但保留，可经 `/history` 回溯时间线；
- `moods`：情绪时间线，从每条消息抽取的情绪（label/valence/note）按时间排列。

情绪识别折叠进"记忆巩固"那一次廉价模型调用里（不额外耗 token），压缩时从整批对话中提炼明显流露的情绪。每轮对话前会把近期情绪趋势压成一行注入上下文，让助手自然体察用户状态。回看窗口是常量 `MOOD_TREND_DAYS`（7 天）。

所有记忆操作都按 `user_id` 隔离。遗忘采用软删除，节点仍可审计但不会再被检索。

每条记忆带 `subject` 区分主体：`user`（关于用户的事实/偏好，自动筛选只产出这类）与 `assistant`（助手自己对用户的承诺、约定或人设设定）。检索时按主体分组呈现给模型、互不混淆；写入会按主体隔离去重；旧数据无该字段时默认视为 `user`。

### 检索：候选池 + 记忆模型精选

检索路径没有向量、没有倒排索引、没有本地模型，就两步：

1. **候选池**：从 `memories` 取该用户的活跃记忆，按 `last_seen_at` 倒序最多 `MEMORY_SELECT_POOL_MAX` 条（默认 400），只取 id/正文/kind/subject。超出上限时截断保留最近被提及的那部分——没有语义信息可用时，新近度是最合理的取舍。
2. **精选**：整池编号后交给记忆模型（`deepseek-v4-flash`），让它返回相关记忆的编号，再按编号映射回真实 id、取回正文（最多 `MEMORY_SEARCH_LIMIT` 条，顺序即模型给出的相关性顺序）。

几个刻意的设计：

- **用行号而非 uuid 指代记忆**。一个 uuid 要十几个 token，几百条候选光 id 就能吃掉上万 token，而模型只需要一个能指回来的编号。越界或重复的编号一律丢弃，模型编不出不存在的记忆。
- **候选池不超过 `MEMORY_SEARCH_LIMIT` 时直接全返，不调模型**。新用户和记忆很少的用户完全不付这份成本。
- **永不因检索失败中断对话**（与它取代的重排器同一原则）：模型调用失败、返回不可解析、编号全部越界，一律回退到「按新近度取前 N 条」并打一条 warn。模型明确返回空数组则尊重它，返回空——那是「确实没有相关记忆」的正常结果，不该硬塞最近几条。
- **记忆清单放在 system 段、查询放在 user 段**。追加式记忆下清单是稳定前缀，支持 prompt caching 的供应商可以整段命中缓存，每轮只有末尾的查询在变。

代价说清楚：每轮对话多一次 LLM 往返。它与历史/摘要/情绪的查询并发（`tokio::join!`），但整轮延迟会被它拖住，量级取决于记忆模型的速度（flash 且关思考，通常是秒级）。换来的是真正的语义理解——指代消解、否定判别、跨措辞匹配，这些是关键词检索在中文闲聊里做不到、而向量检索也只能部分做到的。

`MemoryView.score` 字段在这条路径下不再有值（没有可比的数值分数），序列化时直接省略。

## 资源建议

- CPU 部署：1 核起步。进程里只有 axum + SQLite，没有本地推理也没有图片解码，**稳态匿名内存约 20–40 MB**（含 4 条 SQLite 连接各 512 KiB 页缓存、rustls 根证书库、tokio 运行时）。`mem_limit` 默认 `256m`，峰值只剩 `fetch_url` 的 5 MiB HTML 进 DOM 时那几十 MB 的瞬时占用。
- 磁盘：镜像百 MB 级，数据库按聊天量增长，预留 1 GB 绰绰有余。
- 真正的成本在 token 而不是内存：每轮对话多一次记忆精选调用，输入约等于候选池大小。记忆多到觉得贵时，调小 `MEMORY_SELECT_POOL_MAX` 或 `MEMORY_SELECT_TEXT_MAX_CHARS`；DeepSeek 的 prompt caching 能显著摊薄这部分（候选清单是稳定前缀，命中价约为未命中的 1/50）。

## 备份与更新

SQLite 数据库保存在 Docker volume `app_data`（唯一需要持久化的卷）。备份只需拷出 `/data/memory.db` 一个文件。不要把 `docker compose down -v` 当成普通停止命令；日常停止使用：

```sh
docker compose stop
```

> **从带向量的旧版本升级**：检索改成记忆模型精选后，向量存储不再有任何读者。首次启动会尝试删掉 `vec_memories` 虚拟表、并 `DROP` 更早版本残留的 `memories.embedding` 列，**不可逆**——升级前先备份：`cp memory.db memory.db.bak`。记忆正文本身完好无损，不需要重新生成任何东西。
>
> 本进程不再注册 sqlite-vec 扩展，所以 `DROP TABLE vec_memories` 会因「no such module: vec0」失败，日志里只会留一条 debug。这没有影响：留下的只是一张再没有语句引用的死表，占点磁盘而已。真想清干净，用带 vec0 扩展的 `sqlite3` 手动 DROP 一次即可。

查看错误：

```sh
docker compose ps
docker compose logs --tail=200 agent
```

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

`main` 分支通过测试后，GitHub Actions 会发布一个 `linux/amd64` 镜像（单个 Rust 二进制，包含 API、存储与 QQ 桥接）：

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
