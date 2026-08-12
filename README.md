# Mneme

面向单用户、可长期运行在小型 VPS 上的轻量个人陪伴 Agent。单进程同时提供 QQ 官方机器人 WebSocket 接入和简单 HTTP API，聊天记录与长期记忆保存在 SQLite。

## 功能

- QQ C2C 私聊消息
- QQ WebSocket 长连接和自动重连
- 固定使用 DeepSeek 官方 API 的 `deepseek-v4-pro`
- 默认启用思考模式，`reasoning_effort=max`
- Tavily `web_search`，仅在需要实时外部信息时调用
- SQLite 最近聊天记录和 FTS5 长期记忆
- `/remember`、`/memories`、`/forget`
- `POST /chat`、`GET /health`、`GET /memories`

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

`system` 和 `persona` 是必填项，直接写在 `.env` 中。需要换行时可在值中写 `\n`，程序会将其转换成实际换行。不要把含真实密钥的 `.env` 提交到仓库。

DeepSeek 的地址、模型和思考等级固定在程序中，不再使用 `LLM_BASE_URL`、`LLM_MODEL` 等通用配置：

- API：`https://api.deepseek.com`
- 模型：`deepseek-v4-pro`
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

## QQ 开放平台

在 QQ 开放平台创建机器人并取得 AppID、AppSecret，然后：

1. 为机器人开通 `C2C_MESSAGE_CREATE` 私聊消息事件。
2. 在沙箱阶段添加测试成员。
3. 如果平台要求 OpenAPI IP 白名单，把 VPS 的公网出口 IP 加入白名单。
4. 确认该机器人仍有 WebSocket 事件链路权限。

注意：[QQ 官方 `botgo` 仓库](https://github.com/tencent-connect/botgo)已说明 WebSocket 链路停止维护，原有机器人仍可使用，但新机器人可能没有该权限。程序会按要求使用 WebSocket；如果平台返回未授权 intent 或无法取得 gateway，需要在 QQ 开放平台确认资格。

## Prompt 分层

聊天上下文按以下顺序发送给 DeepSeek：

1. `.env` 中的 `system`：系统行为和工具规则
2. `.env` 中的 `persona`：陪伴者的人格、语气和关系风格
3. 当前日期和本轮相关长期记忆
4. 当前会话最近聊天与用户消息

长期记忆判断使用独立的 `prompts/memory.txt`，不会让 persona 干扰结构化记忆维护。

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

数据保存在宿主机 `./data`。HTTP API 默认只映射到宿主机 `127.0.0.1:8787`；QQ WebSocket 是容器主动向外连接，不需要开放额外入站端口。

## 本地构建

```bash
CGO_ENABLED=0 go build -o companion ./cmd/companion
```

终端模式需要 `DEEPSEEK_API_KEY` 和 `TAVILY_API_KEY`：

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
curl -X POST http://127.0.0.1:8787/chat \
  -H 'Content-Type: application/json' \
  -d '{"message":"今天有什么 AI 新闻？"}'
```

HTTP 服务没有内置鉴权，不要直接暴露到公网；需要公网访问时应放在带 HTTPS 和认证的反向代理后。
