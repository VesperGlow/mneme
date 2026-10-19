# mneme-tools

`mneme-tools` 是独立镜像 `ghcr.io/vesperglow/mneme-tools:latest`，在已有 **mneme.pod** 内增加一个容器。`mneme-api` 继续编排对话、记忆和 LLM Tool Calling；`mneme-models` 继续运行原有 Embedding/Reranker；`mneme-neo4j` 保持原样。不会创建额外 Pod，也不会部署视觉模型。

## 启用与升级

填写原有 `.env`，并增加：

```dotenv
MNEME_TOOLS_ENABLED=true
MNEME_TOOLS_TOKEN=
# 单个 key 直接填写；多个 key 用 / 分隔。
MNEME_TAVILY_API_KEY=tvly-key1/tvly-key2
MNEME_TOOLS_MAX_CALLS=200
MNEME_TOOLS_TURN_TIMEOUT_SECONDS=1800
```

使用支持 **Chat Completions 工具调用及图片输入** 的现有 LLM，建议显式设置 `MNEME_LLM_MODEL`。安装脚本生成独立工具凭证，写回权限 0600 的 `.env`、`app.env`；Tavily key 自动传入工具容器使用的 `~/.config/mneme/tools.env`，该文件只包含工具凭证与 Tavily key，权限为 0600。Tavily key 不写入 API 的 `app.env`。重复安装保留已有 key，显式设置 `MNEME_TAVILY_API_KEY=` 可清除。旧 `.env` 未指定 `MNEME_TOOLS_ENABLED` 时保持工具关闭，避免把不支持工具调用的已有模型直接切换到新协议；新 `.env.example` 已显式启用。

```bash
bash scripts/pull.sh
bash scripts/install.sh
systemctl --user restart mneme.target
```

源码构建使用 `bash scripts/build.sh`。三个镜像均支持 `MNEME_IMAGE_TAG=sha-完整提交SHA`。新增 Quadlet `mneme-tools.container` 使用 `Pod=mneme.pod`；现有 Pod、网络及 API/模型/图谱容器配置不变。target 通过 Wants 启动工具容器，工具启动失败不会成为原有核心服务的硬依赖。`MNEME_TOOLS_ENABLED=false` 保留原有纯文本生成路径。

原生开发可用 Node.js 22+（镜像与 CI 使用 Node 24）、系统 Chromium 和 `npm ci --prefix services/tools`：

```bash
MNEME_TOOLS_TOKEN=本机独立随机工具凭证 node services/tools/app.mjs
# 从项目根目录启动会自动读取 .env 中的 Tavily key。
# API 使用相同工具凭证，并设置 MNEME_TOOLS_ENABLED=true。
```

默认连接 `http://127.0.0.1:8091`，可用 `MNEME_TOOLS_URL` 覆盖 API 的连接地址。生产服务仅监听 Pod 内 `127.0.0.1:8091`，**不发布工具、代理或 CDP 端口**。工具服务不需要 LLM Key、API Token、Neo4j 密码、主数据库卷或容器管理 socket。

## 浏览器与多轮调用

`browser` 的 `search` 动作调用 [Tavily Search API](https://docs.tavily.com/documentation/api-reference/endpoint/search)，返回标题、来源 URL、摘要和读取时间。只需在项目 `.env` 设置 `MNEME_TAVILY_API_KEY` 并执行上述安装与重启命令；无需修改 `config.toml` 或 Quadlet。单个 key 直接填写，多个 key 用 `/` 分隔，例如 `tvly-key1/tvly-key2/tvly-key3`。工具服务在所有会话之间按搜索请求轮转：key1 → key2 → key3 → key1；在网络请求前预留序号，支持并发。失败请求也占用一次轮转，下一次搜索使用下一个 key；同一次搜索不自动重试。重启服务后从第一个 key 重新开始。

未配置 key 时仅搜索返回 `tavily_api_key_required`，Chromium 的网址浏览、读取和截图等功能仍可使用。Tavily 超时、凭证无效、限流和额度耗尽分别返回 `tavily_timeout`、`tavily_auth_failed`、`tavily_rate_limited` 和 `tavily_quota_exceeded`。日志、工具结果及健康接口不返回 key 或上游错误正文。`/health` 的 `engine` 为 `tavily`，`search_configured` 表示是否填入 key，不代表已验证额度或有效性。

搜索不会打开或改变浏览器页面，也不会创建浏览器会话。模型拿到搜索结果后，通过 `navigate(url)` 打开所选来源，再使用 `read`、`click`、`type`、`scroll`、`back`、`wait`、`screenshot` 或 `close`。网页浏览仍通过 Playwright 的 `connectOverCDP` 控制真实 Headless Chromium，正文提取复用 Mozilla Readability。普通导航遇到验证页、403 或 429 时分别返回 `page_verification_required`、`page_access_denied` 或 `page_rate_limited`。旧的 `MNEME_SEARCH_ENGINE` 不再用于选择搜索引擎。

导航在新文档提交后，最多等待 4 秒的 DOMContentLoaded 和 2.5 秒的正文渲染；慢脚本或持续传输的页面可返回已加载正文，并标记 `load_incomplete`，无可读正文及元素时标记 `rendering_pending`。元素引用在一次 DOM 快照中批量生成，避免逐元素 CDP 往返。浏览器启动和会话创建分别有 10 秒超时，会话创建耗时计入单次操作的 25 秒预算；超时后才返回的上下文也会被关闭。普通操作超时保留页面，便于 `read`、`wait` 或换站恢复；整个调用超过 25 秒时才关闭上下文以取消挂起任务。会话不存在或已回收时，读取和交互明确返回 `browser_session_missing_navigate`，必须重新导航，不会返回空白页假装读取成功。错误日志包含阶段和会话是否已关闭，便于区分操作、内容提取与截图超时。

统一工具目录位于 `services/tools/catalog.json`，HTTP 服务与 Rust Agent 共用。`browser` 支持：

| action | 参数 / 结果 |
|---|---|
| search | query；通过 Tavily 搜索，返回 results（title、url、snippet），不打开页面 |
| navigate | url；访问公网 HTTP/HTTPS 网页 |
| read | 读取当前动态 DOM，刷新元素引用 |
| click | element，例如 e1；点击上一次快照中的元素 |
| type | element、text、可选 press_enter；仅输入普通文本，拒绝密码和文件输入框 |
| scroll | 可选 delta_y，范围 -4000 到 4000 |
| wait | 可选 wait_ms，上限 3000 毫秒 |
| screenshot | 当前视口 JPEG，同时供现有多模态模型查看 |
| back / close | 返回上一页 / 释放当前浏览器会话 |

Rust 解析分片 SSE 中的 tool_calls，等完整 `[DONE]` 后才执行。工具结果通过匹配的 `tool_call_id` 返回模型，模型可继续选择下一步。参数错误、浏览器故障与 SSRF 拒绝均以结构化错误返回，供模型纠正；工具调用次数和总时间有硬上限。对话断连继续完成的既有语义不变；相同 request_id 重放不会再次点击或搜索。图片及其顺序也参与请求幂等检查。

已执行工具后，后续模型请求失败或流中断时，会将已有工具结果交给一次不带工具调用历史的纯文本请求收尾，不重做浏览器操作。如果收尾请求也失败，仍保存并返回明确的失败说明和已读取的来源，避免整轮浏览只有通用生成失败提示。

默认每轮最多 200 次调用、整轮最长 1800 秒。已有部署的 `config.toml` 会保留旧值；在项目 `.env` 或 API 的 `~/.config/mneme/app.env` 中设置上述两个限制变量即可覆盖，工具容器的 `tools.env` 保存工具凭证与 Tavily key。次数范围 1–200，总时限范围 10–3600 秒。

达到调用上限时，API 将已完成的结果转换成有界的不可信资料，发起 `tool_choice=none` 的纯文本收尾请求，不再携带原生工具调用历史，避免 [DeepSeek Flash 已有的 DSML 泄漏复现](https://github.com/deepseek-ai/DeepSeek-V3/issues/1678)。流式输出在标记跨分片时也会拦截 DSML，不把其当成工具命令执行或作为答复保存；收尾仍失败时返回可读说明和已读取的来源。思考模型返回的 reasoning_content 仅在本轮后续工具请求中回传，不出现在可见回复或工具日志中。

工具返回的网页是非可信资料，独立于人格提示词和用户原话。服务不提供 shell、文件读取或任意 JavaScript 执行接口。浏览器交互面向网页阅读；页面只允许 GET/HEAD 和下述受限 JSON POST，搜索通过独立的 Tavily HTTPS 请求完成。工具结果会进行长度限制，旧工具轮次按上下文预算裁剪。网页内容不会作为用户原话直接提取进长期记忆。

## 图片与 Console

Console 新增「对话」及「工具记录」。对话页支持新建/继续会话、流式回复、上传图片、显示执行中状态、来源链接和图片预览。工具页显示健康状态、浏览器会话数、调用统计、时间、错误和结构化结果，可释放指定会话。原有记忆 CRUD、性格设置、密码与重置继续使用原接口。

每轮最多 4 张 PNG/JPEG/WebP，每张最多 8 MiB、解码不超过 2000 万像素。工具服务使用 Sharp 验证图片、纠正旋转、移除元数据、缩至最长 2048 像素并转为 JPEG；不接收 SVG。截图和上传图片保存在 SQLite `assets` 中，总额最多 256 MiB。未关联消息或工具记录的上传图片 24 小时后由定期维护清理；已关联图片随会话保留，重置 Agent 时一并删除。

API 把图片作为 `image_url` 的 data URL 发给当前多模态 LLM。工具消息本身保持文本格式，截图作为后续 user 图片消息附加。`view_image(asset_id)` 可读取当前会话中既有上传图片或截图，拒绝跨会话读取。模型无需下载访问 Token，也不经浏览器联网读取上传图片。后续对话会得到历史图片标识，可按需重新查看。上传图片和截图的理解质量取决于所配置的提供商；当前不自动探测模型视觉能力。QQ 支持图文和纯图片消息，沿用同一规范化与视觉输入链路；也可以发送自定义表情包及当前对话中已有的上传图片 / 截图，见 [QQ 图片说明](qq.md#图片与自定义表情包)。

## 接口

以下接口在 tools 容器中仅面向 API，全部要求独立 `Authorization: Bearer <MNEME_TOOLS_TOKEN>`：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | /health | Chromium、沙箱、会话、Tavily 配置状态和统计 |
| GET | /v1/tools | 共用工具目录 |
| POST | /v1/execute | `{"session_id":"UUID","arguments":{"action":"search","query":"Debian 官网"}}` |
| DELETE | /v1/sessions/{id} | 释放浏览器上下文 |
| POST | /v1/reset | 关闭全部浏览器上下文 |
| POST | /v1/images/normalize | application/octet-stream 图片，返回规范化 JPEG；GIF 使用首帧 |
| POST | /v1/stickers/normalize | 原始图片，返回最长 512 像素的表情素材，保留透明和动画 |
| POST | /v1/images/fetch | `{"url":"公网附件地址"}`，验证每次解析与重定向，限 8 MiB / 15 秒，返回规范化 JPEG |

以下接口由 mneme-api 暴露，使用原 API Token 或 Console 密码会话：

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | /v1/sessions | 最近 50 个会话及摘要 |
| POST | /v1/sessions/{id}/images | 原始图片 body；返回 asset_id |
| POST | /v1/sessions/{id}/chat | 原 message、request_id，新增可选 assets: [UUID] |
| GET | /v1/sessions/{id}/messages | messages，以及 assets 和 tool_calls 元数据 |
| GET | /v1/assets/{id} | 受鉴权保护的 JPEG 图片 |
| GET | /v1/console/tools | 工具服务状态及 API 侧调用统计 |
| GET | /v1/console/tools/calls | 最近 100 条记录，可按 session_id 筛选 |
| POST | /v1/console/tools/close | `{"session_id":"UUID"}`，释放浏览器会话 |

SSE 保留 meta/delta/done/error，新增 tool 事件，包含 id、name、action、status、elapsed_ms、error 和有界结果。工具日志记录 SQLite；密码输入和普通 type 的实际文本不进入工具日志，进程日志只含动作、状态、耗时和固定错误码。原始图片数据不写入日志。

## 共享 Pod 的边界与防护

这是共享网络命名空间方案，**不提供容器之间的网络隔离**。Pod 内进程可以连接彼此的 localhost；CDP 也只能在信任整个 Pod 的前提下使用。浏览器主进程一旦被完全攻破，应用代理不能替代内核级网络策略。若将来需要把浏览器主进程也视为不可信主体，必须改用独立网络命名空间或受控出口网关，不能宣称共享 Pod 已达到同样隔离。

当前防护针对不可信网页触发的访问：

- 独立工具凭证、严格 JSON schema、32 KiB 工具参数限制，不允许浏览器 URL 中带账号密码，不把主服务凭证或 Tavily key 传入 Chromium 环境。Tavily 请求只发送至固定的 `https://api.tavily.com/search`，禁止跟随重定向，时限 25 秒，响应上限 1 MiB。
- 浏览器所有 HTTP/HTTPS 流量经过本地代理。每次连接重新解析 DNS、检查全部地址，只允许公网单播地址的 80/443 端口，并连接已检查的 IP，避免 DNS 重绑定；跳转和子资源同样检查。回环、私网、链路本地、云元数据、IPv4 映射地址及保留网段被拒绝。
- 页面可使用 GET/HEAD，以及初始化动态页面所需的 JSON POST；后者仅限 fetch/XHR、有效 JSON、请求体不超过 64 KiB、不含 Authorization。其余表单提交、非 JSON POST 及 PUT/PATCH/DELETE 等方法被拒绝。JSON POST 也可能改变公网网站上的状态；这是匿名浏览器页面脚本的有限网络能力，不等于严格只读的网络沙箱，所有目标仍需通过公网地址校验。
- 禁用隐式 loopback 代理绕过、QUIC、非代理 WebRTC、WebSocket、Service Worker 和下载；页面中的 WebRTC/WebTransport 构造器也禁用。不开放任意 CDP 命令给模型。
- 非 root 用户运行，Chromium user namespace 沙箱始终启用，启动失败即报错，生产没有 --no-sandbox 降级。定制 seccomp 仅为沙箱允许 clone/clone3/unshare/setns/chroot，保留其余默认过滤；不增加 SYS_ADMIN、privileged、宿主网络或 IPC 权限。
- 容器只读根目录、全部 capabilities 移除、no-new-privileges；/tmp 512 MiB，PIDs（含线程）512，内存 1536 MiB，CPU 上限 150%。内存及 CPU 通过容器参数限制实际工作负载，不能只限制启动 Podman 的 systemd 服务。浏览器启动后关闭未使用的空白标签页，减少渲染进程占用；工具临时浏览器文件随上下文/进程清理，不挂载主数据卷。
- 最多 4 个浏览器上下文、同会话串行，最多 4 个并发重操作、128 个代理连接；动作超时 25 秒，空闲 10 分钟回收，每 15 秒检查。API 默认每轮最多 200 个工具调用、每请求 30 秒、整轮 1800 秒，可在 [tools] 或相应环境变量中调整有界限制。

Ubuntu 等宿主机的 AppArmor/用户命名空间策略可能阻止沙箱。此时应修正宿主机策略，不能通过关闭沙箱或赋予宿主机管理权限来运行。Debian 13 rootless Podman 已做实际验证。升级前备份 SQLite，启动时自动执行 0004_tools.sql；原有迁移不改写。Agent 重置会删除图片/调用记录并设置浏览器清理标记，后续第一次工具操作先关闭旧上下文，工具离线不会丢失清理需求。

## 验证

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
npm ci --prefix services/tools
node --test services/tools/test/*.test.mjs
MNEME_BROWSER_TEST=1 node --test services/tools/test/browser.test.mjs
# 构建镜像后，验证共享 Pod 的 HTTP 鉴权、沙箱、内部服务拦截与 CDP 动态交互：
bash scripts/smoke-tools.sh
# 宿主机需要 Chromium、Node 22+；临时 API 监听 18080，请确保空闲。
# Tavily 搜索协议替身 + 真实浏览器 + Rust 多轮 Tool Calling + 图片 + 桌面/手机 Console：
node scripts/smoke-tools.mjs
```

最后一项使用真实公网浏览器和本地确定性 LLM 协议替身，不能据此声称某个真实提供商的工具选择或视觉质量已验收。搜索默认使用确定性 Tavily 协议替身；设置 `MNEME_QA_TAVILY_API_KEY` 可显式使用真实 Tavily key 验证公网搜索。普通网页浏览仍依赖公网。验证产物位于忽略的 artifacts/tools-validation.json、tools-browser-live.jpg、tools-console-desktop.png、tools-console-chat.png 和 tools-console-mobile.png。测试默认不使用生产配置、密钥或数据库。

实现依据：[Playwright CDP](https://playwright.dev/docs/api/class-browsertype#browser-type-connect-over-cdp)、[Chromium 沙箱容器配置](https://playwright.dev/docs/docker)、[Mozilla Readability](https://github.com/mozilla/readability)、[Chat Completions 工具调用](https://developers.openai.com/api/docs/guides/function-calling)。
