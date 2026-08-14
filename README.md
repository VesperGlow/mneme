# Mneme

面向单用户、可长期运行在小型 VPS 上的轻量个人陪伴 Agent。单进程同时提供 QQ 官方机器人 WebSocket 接入和简单 HTTP API，聊天记录与长期记忆保存在 SQLite。

## 功能

- QQ C2C 私聊消息
- QQ WebSocket 长连接和自动重连
- 核心聊天固定使用 DeepSeek `deepseek-v4-pro`，思考模式 `reasoning_effort=max`
- 每轮回复前由 `deepseek-v4-flash` 理解消息并按需检索长期记忆，最终回复仍由 `deepseek-v4-pro` 生成
- 后台记忆整理和阶段摘要同样使用 `deepseek-v4-flash`
- Tavily `web_search` 与 `open_url`：搜索实时外部信息，或读取并总结指定网页链接
- QQ 工具进度回复：先发送模型的简短查询提示，工具完成后再单独发送最终答案
- 分层上下文：最近消息、阶段摘要、结构化 FTS5 长期记忆
- 有界后台记忆队列、渠道消息去重、定期备份和 JSON 导出
- `/remember`、`/memories`、`/memory`、`/correct`、`/forget`、`/export`
- `POST /chat`、`GET /health`、`GET /memories`、`GET /export`

除 QQ 官方 `botgo` 和纯 Go SQLite 驱动外，程序不使用 Agent 框架、Redis、PostgreSQL 或向量数据库。

## 配置

复制环境变量示例：

```bash
cp .env.example .env
```

填写：

```dotenv
QQ_APP_ID=你的机器人AppID
QQ_APP_SECRET=你的机器人AppSecret
DEEPSEEK_API_KEY=你的DeepSeek API Key
TAVILY_API_KEY=你的Tavily API Key
system="系统指令"
persona="陪伴者的人格、语气和关系风格"
```

`system` 和 `persona` 是必填项，直接写在 `.env` 中。需要换行时可在值中写 `\n`，程序会将其转换成实际换行。不要把含真实密钥的 `.env` 提交到仓库。`TAVILY_API_KEY` 可选；未配置时联网搜索和打开链接均不可用。

QQ 不会渲染完整 Markdown。示例 `system` 已要求模型只输出纯文本，避免把 `**`、代码围栏等符号直接显示给用户。

DeepSeek 的地址、模型和思考等级固定在程序中，不再使用 `LLM_BASE_URL`、`LLM_MODEL` 等通用配置：

- API：`https://api.deepseek.com`
- 聊天模型：`deepseek-v4-pro`，reasoning effort 为 `max`
- 记忆模型：`deepseek-v4-flash`，reasoning effort 为 `high`
- thinking：`enabled`
- reasoning effort：`max`

其他可选变量：

| 变量 | 默认值 |
| --- | --- |
| `COMPANION_DB` | `./data/companion.db` |
| `COMPANION_ADDR` | `127.0.0.1:8787` |
| `COMPANION_RECENT_MESSAGES` | `20` |
| `COMPANION_MAX_MEMORIES` | `5` |
| `COMPANION_MAX_TOOL_CALLS` | `3` |
| `COMPANION_REQUEST_TIMEOUT_SECONDS` | `120` |
| `COMPANION_MEMORY_QUEUE_SIZE` | `32` |
| `COMPANION_SUMMARY_EVERY` | `20`（对话轮数；设为 `0` 可关闭） |
| `COMPANION_BACKUP_DIR` | 数据库同目录下的 `backups` |
| `COMPANION_BACKUP_INTERVAL_HOURS` | `24` |
| `COMPANION_BACKUP_RETENTION_DAYS` | `14` |
| `COMPANION_LOG_FORMAT` | `pretty`（可设为 `json`） |
| `COMPANION_LOG_LEVEL` | `info`（`debug`、`info`、`warn`、`error`） |

## QQ 开放平台

在 QQ 开放平台创建机器人并取得 AppID、AppSecret，然后：

1. 为机器人开通 `C2C_MESSAGE_CREATE` 私聊消息事件。
2. 在沙箱阶段添加测试成员。
3. 如果平台要求 OpenAPI IP 白名单，把 VPS 的公网出口 IP 加入白名单。
4. 确认该机器人仍有 WebSocket 事件链路权限。

注意：[QQ 官方 `botgo` 仓库](https://github.com/tencent-connect/botgo)已说明 WebSocket 链路停止维护，原有机器人仍可使用，但新机器人可能没有该权限。程序会按要求使用 WebSocket；如果平台返回未授权 intent 或无法取得 gateway，需要在 QQ 开放平台确认资格。

## Prompt 分层

每轮普通消息先经过记忆检索，再进入聊天模型：

1. `deepseek-v4-flash` 根据当前消息和最近对话判断是否需要长期记忆，并通过内部 `search_memories` 工具生成、调整检索词。
2. Flash 从实际搜索结果中选出本轮真正相关的记忆；检索失败或超时时自动退回原句 FTS5 检索，不中断聊天。
3. `.env` 中的 `system`、`persona`、当前日期、阶段摘要、筛选后的长期记忆、最近聊天和用户原始消息一起发送给 `deepseek-v4-pro`。
4. `deepseek-v4-pro` 可按需调用 `web_search` 搜索互联网，或调用 `open_url` 读取用户提供及搜索结果中的具体网页。网页内容会被标记为不可信外部资料，并限制长度后再返回模型。
5. `deepseek-v4-pro` 负责工具使用和最终回复；如果它在调用工具前生成了简短查询提示，QQ 会立即把提示作为一条进度消息发送，工具完成后再发送最终答案。若模型只回复“让我查查”却没有实际调用工具，Agent 会保留该回复并追加一次完成请求的重试。Flash 不生成面向用户的回答。

QQ、HTTP 和终端只是同一个个人 Agent 的入口，不会创建彼此隔离的人格或记忆。带消息 ID 的渠道会自动去重。检索规划和长期记忆维护分别使用独立的 `prompts/retrieval.txt` 与 `prompts/memory.txt`，不会让 persona 干扰记忆层；自动记忆包含类型、重要度、置信度和来源，手动 `/remember` 的内容会固定，避免被后台任务静默改写。

阶段摘要每累计默认 20 轮对话更新一次，用来保留近期进行中的事情；稳定事实仍由长期记忆维护。

## 记忆命令

```text
/remember 要长期记住的内容
/memories
/memories 检索词
/memory #12
/correct #12 更新后的完整内容
/forget #12
/forget 模糊检索词
/export
```

优先使用带 ID 的 `/forget #12`，避免模糊检索同时停用多条记忆。

## 容器运行

GitHub Actions 会构建 `linux/amd64` 和 `linux/arm64`：

```text
ghcr.io/vesperglow/mneme:latest
```

启动：

```bash
docker compose up -d
docker compose logs -f companion
```

数据保存在宿主机 `./data`。程序启动时会执行 SQLite 快速完整性检查，并在 `./data/backups` 中创建定期一致性备份，默认保留 14 天。HTTP API 默认只映射到宿主机 `127.0.0.1:8787`；QQ WebSocket 是容器主动向外连接，不需要开放额外入站端口。

## 运行日志

Mneme 向标准错误输出单行结构化日志，Docker 和 systemd 都可以直接收集。默认的 `pretty` 格式把时间、级别、组件和事件分列显示，字段统一使用简短的 `snake_case` 名称，耗时自动显示为 `ms` 或 `s`：

```text
2026-08-14 21:16:42.103  INFO  agent     │ 收到聊天  request=7 channel=qq input_chars=18 external_message_id=true
2026-08-14 21:16:42.945  INFO  agent     │ 记忆检索完成  request=7 strategy=flash selected=2 duration=842ms
2026-08-14 21:16:43.555  INFO  agent     │ 工具调用完成  request=7 tool=open_url content_chars=1530 duration=610ms
2026-08-14 21:16:43.735  INFO  agent     │ 进度消息已发送  request=7 output_chars=12 duration=180ms
2026-08-14 21:16:45.865  INFO  agent     │ 聊天生成完成  request=7 model=deepseek-v4-pro rounds=2 tools_used=1 output_chars=96 duration=2.31s
2026-08-14 21:16:46.540  INFO  agent     │ 记忆复核完成  request=7 changes=1 duration=675ms
2026-08-14 21:16:46.929  INFO  qq        │ 回复已发送  transport=7 output_chars=96 chunks=1 duration=3.89s
```

`request` 表示 Agent 内的一轮处理，`transport` 表示 QQ 接入层收到的一条消息。默认 `info` 级别保留关键生命周期与结果；将 `COMPANION_LOG_LEVEL` 设为 `debug` 可查看队列、模型轮次和处理中事件。交互式终端会为级别着色，重定向、Docker 和 systemd 收集时自动禁用 ANSI 颜色；设置 `NO_COLOR=1` 也可强制禁用。

如果日志需要交给 Loki、Elasticsearch 等系统处理，可切换为一行一个对象的 JSON：

```dotenv
COMPANION_LOG_FORMAT=json
```

日志不会写入聊天正文、长期记忆正文、搜索词、网页 URL、用户 ID、系统 Prompt 或密钥，只记录字符数、数量、状态和耗时。

查看容器日志：

```bash
docker compose logs -f companion
```

由 systemd 托管时：

```bash
journalctl --user -u mneme.service -f -o cat
```

如果 service 使用 `docker run -d`，启动时单独出现的一串 64 位十六进制字符是 Docker 返回的容器 ID，不是 Mneme 业务日志。可在 unit 的启动脚本中隐藏该命令输出，Mneme 自身日志不受影响。

## 数据、备份与迁移

Mneme 当前以本地 SQLite 为唯一主存储，不接入 S3 或其他对象存储后端。运行数据库位于 `./data/companion.db`，定期一致性备份位于 `./data/backups/mneme-*.db`。JSON 导出适合人工查看和长期归档，但不代替 SQLite 数据库备份。

迁移到另一台机器时：

1. 在旧机器执行 `docker compose down`，等待容器正常退出。
2. 将 `.env`、`compose.yaml` 和整个 `./data` 目录复制到新机器。
3. 在新机器保持相同目录结构，执行 `docker compose up -d`。
4. 使用 `docker compose logs companion` 和 `curl http://127.0.0.1:8787/health` 检查启动与数据库完整性。

不要在容器仍运行时只复制 `companion.db`，也不要把活动中的 SQLite 文件直接放在对象存储或不可靠的网络文件系统上。需要从定期备份恢复时，先停止容器，在一个空的数据目录中把选定的 `mneme-*.db` 复制为 `companion.db`，再启动容器。

## 本地构建

```bash
CGO_ENABLED=0 go build -o companion ./cmd/companion
```

终端模式需要 `DEEPSEEK_API_KEY`；联网搜索和打开链接需要 `TAVILY_API_KEY`：

```bash
./companion chat
```

同时启动 QQ WebSocket 和 HTTP API：

```bash
./companion serve
```

## HTTP API

```bash
curl http://127.0.0.1:8787/health
curl http://127.0.0.1:8787/memories
curl -o mneme-export.json http://127.0.0.1:8787/export
curl -X POST http://127.0.0.1:8787/chat \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: 自定义的唯一消息ID' \
  -d '{"message":"今天有什么 AI 新闻？"}'
```

HTTP 服务没有内置鉴权，不要直接暴露到公网；需要公网访问时应放在带 HTTPS 和认证的反向代理后。
