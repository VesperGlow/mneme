# Companion

一个面向单用户、可长期运行在小型 VPS 上的轻量个人陪伴 Agent。它直接调用 OpenAI Compatible Chat Completions API 和 Tavily HTTP API，不使用 Agent 框架、Redis、向量数据库等服务。

## 特点

- 最近聊天记录与长期记忆均保存在 SQLite
- 每轮只读取最近 20 条消息和最多 5 条 FTS5 相关记忆
- 模型可新增、修改或失效真正长期有效的记忆
- 支持 `/remember`、`/memories`、`/forget`
- 仅在需要实时或外部信息时由模型调用 `web_search`
- 工具调用默认最多 3 次，避免无限循环
- 一个纯 Go SQLite 驱动是唯一的非标准库依赖，可在 `CGO_ENABLED=0` 下构建

## 配置

必需环境变量：

```bash
export LLM_BASE_URL="https://api.openai.com/v1"
export LLM_API_KEY="your-key"
export LLM_MODEL="your-model"
```

如需联网搜索，再配置：

```bash
export TAVILY_API_KEY="tvly-your-key"
```

可选配置及默认值：

| 变量 | 默认值 |
| --- | --- |
| `COMPANION_DB` | `./data/companion.db` |
| `COMPANION_ADDR` | `127.0.0.1:8787` |
| `COMPANION_RECENT_MESSAGES` | `20` |
| `COMPANION_MAX_MEMORIES` | `5` |
| `COMPANION_MAX_TOOL_CALLS` | `3` |
| `COMPANION_REQUEST_TIMEOUT_SECONDS` | `120` |

`LLM_BASE_URL` 可以是类似 `https://host/v1` 的 API 根地址，也可以直接是完整的 `/chat/completions` 地址。

## 构建与运行

```bash
go mod download
CGO_ENABLED=0 go build -o companion ./cmd/companion
./companion chat
```

启动 HTTP 服务：

```bash
./companion serve
```

## 容器运行

每次推送到 `main` 后，GitHub Actions 会运行测试并构建 `linux/amd64`、`linux/arm64` 镜像，发布到：

```text
ghcr.io/vesperglow/mneme:latest
```

在 VPS 上创建 `.env`（不要提交到 Git）：

```bash
LLM_BASE_URL=https://api.openai.com/v1
LLM_API_KEY=your-key
LLM_MODEL=your-model
TAVILY_API_KEY=tvly-your-key
```

然后启动：

```bash
docker compose up -d
```

数据会持久化到宿主机的 `./data`。Compose 默认只把服务映射到宿主机的 `127.0.0.1:8787`。

接口示例：

```bash
curl http://127.0.0.1:8787/health
curl http://127.0.0.1:8787/memories
curl -X POST http://127.0.0.1:8787/chat \
  -H 'Content-Type: application/json' \
  -d '{"message":"今天有什么 AI 新闻？"}'
```

终端和 `/chat` 都接受以下命令：

```text
/remember 我喝咖啡不加糖
/memories
/forget 咖啡
```

`/forget` 会把最多 5 条 FTS5 匹配记忆标记为失效，不物理删除，便于以后检查数据库和恢复。

## HTTP API

- `POST /chat`：请求体 `{"message":"..."}`，返回 `{"reply":"..."}`
- `GET /health`：检查进程与数据库
- `GET /memories?limit=100`：列出有效长期记忆

HTTP 服务默认只监听本机回环地址。若需要从公网访问，建议放在带 HTTPS 和鉴权的反向代理后，不要直接暴露。
