# Mneme Console

Console 内置在 Mneme 的 Axum 服务中。启动现有服务后打开 **http://127.0.0.1:8080/console/**，使用独立 WebUI 密码登录。安装生成的初始密码位于 `~/.config/mneme/console-password`；已有密码保持不变。无需 Node.js、前端构建步骤、额外端口或独立容器。HTML、CSS、JavaScript 和图标在编译时嵌入 Rust 二进制，不从 CDN 加载脚本、字体或其他资源。

## 功能

- **聊天**（默认入口）：居中消息流、用户气泡、底部自适应输入框；Enter 发送，Shift+Enter 换行，中文输入法选词不会误发。助手支持基础 Markdown 与代码块。单一持续对话、上下文用量与滚动摘要、SSE 流式回复、实时工具状态、来源链接及截图；每条消息最多上传 4 张 PNG/JPEG/WebP，每张最大 8 MiB。图片由工具容器规范化后发送给原有多模态 LLM；需启用工具并配置兼容模型。
- **工具**：桌面左右双栏，左侧服务状态与统计，右侧最近调用记录；窄屏自动上下排列。浏览器健康、沙箱、会话容量、调用次数与平均耗时，最近 100 次调用的状态、结果、来源与错误；可释放指定会话的浏览器上下文。
- **记忆库**：合并原概览，显示记忆总量、有效数量、近 7 天新增、后台任务、趋势图与类型分布；搜索位于本页。在“全部记忆、有效”等筛选旁切换“后台队列”，查看待处理、运行中和失败数量，可重试失败任务或重建索引；队列每 5 秒自动更新，切换保留搜索条件。按内容、关联实体或属性键搜索；按类型和有效/失效/删除状态筛选；按创建、更新时间、重要性、可信度排序；服务器分页。新增与编辑支持内容、类型、重要性、可信度、有效期和最多 8 个关联实体。
- **详情**：原始内容、人工/自动来源、重要性、可信度、时间、实体、索引同步状态、最近 100 条来源消息及最近 100 次相关修订。对话来源可展开查看原始用户消息。旧版本通过相同属性键串联；人工编辑之前的证据仅用于追溯，不自动成为新内容的证明。
- **检索实验室**：使用与实时对话完全相同的管线，展示 Neo4j 向量、CJK 全文、实体匹配、图谱扩展、近期 SQLite 补充候选、RRF 分数、Reranker 分数、最终得分和筛除原因。各通道原始分数不可直接比较；精排服务异常时显示回退状态。删除的内容不会在调试候选中回显。
- **系统状态**：LLM、Neo4j、Embedding、Reranker 的连通/加载状态，探测耗时，当前进程已完成请求的平均耗时、次数、失败数，运行时间、SQLite 大小。后台任务和索引维护统一在记忆库处理。
- **Agent**：统一管理名字、性格、交流方式、边界、输出格式 Prompt，以及 QQ 机器人状态、共享上下文预算和记忆提取周期。性格保存后下一轮对话生效；输入 `RESET` 确认重置，可同时恢复配置文件中的默认性格。
- **设置**：修改 WebUI 登录密码，查看会话有效期。

全站移除顶栏与页面顶部大标题，身份已验证、退出和设置入口位于侧栏底部，使用浏览器刷新页面。桌面端使用侧边导航和详情抽屉，移动端使用可展开侧栏与自适应列表。正文约 16–17px，辅助说明约 13–15px，统一加大按钮、输入框与点击区域，字号使用 rem 并加深辅助文字颜色。支持键盘焦点、原生对话框、Escape 关闭以及 `/` 快速进入搜索。

## 身份与访问

公开页面只包含登录界面和静态资源，私人数据由受保护的 API 加载。WebUI 只使用密码登录，服务端签发 HttpOnly、SameSite=Strict 的持久 Cookie，不向 JavaScript 返回凭证，也不使用 localStorage、sessionStorage 或 URL 保存凭证。登录有效期默认 30 天，每次使用时自动续期；刷新、关闭浏览器和服务重启后继续有效。退出登录、密码变更或过期后重新验证。

`[console].session_days` 可设为 1–365 天（环境变量 `MNEME_CONSOLE_SESSION_DAYS`），HTTPS 反向代理部署时设置 `secure_cookie=true` 或 `MNEME_CONSOLE_SECURE_COOKIE=true`。本机 HTTP / SSH 转发使用默认 false。服务端在 SQLite 仅保存会话 Token 的 SHA-256 哈希；密码使用随机盐与 PBKDF2-HMAC-SHA256（600,000 次）保存。API 使用 `no-store`，CSP 仅允许本站资源，跨站和不同来源的浏览器请求被拒绝。

默认原生服务监听 `127.0.0.1:8080`；现有 Quadlet 仅将 Pod 端口发布到宿主机的 `127.0.0.1`。不需要为了 Console 修改监听或发布设置。远程使用可沿用 SSH 转发：

```bash
ssh -L 8080:127.0.0.1:8080 user@server
```

Console 仍面向单用户，没有多用户角色。新安装自动生成与 API Token 分离的初始密码；原生运行可用 `MNEME_CONSOLE_PASSWORD` 在没有已保存密码时初始化一次。已有 WebUI 密码优先于环境中的初始密码。密码须为 10–128 字符，支持中文和首尾空格。设置页更改密码须填写当前密码，变更会撤销所有设备的登录；不支持移除密码。忘记密码时，通过本机终端用 API Token 调用 `PUT /v1/console/password` 设置新密码，无需在 WebUI 输入 API Token。

## 持续对话与压缩

QQ、API、终端和 WebUI 共用同一段对话，不提供新建和切换窗口。旧 API 和 QQ 会话在启动时合并，原始消息、图片、工具记录和来源保留，旧 ID 指向同一段对话。各渠道共用同一个上下文与生成锁；旧会话 ID 继续作为别名使用，QQ 的 `/新会话` 不会再分叉上下文。

在 `[llm]` 配置 `context_tokens`（默认 100000）和 `summary_chars`（默认 2000）。下一轮会超过 token 预算时，合并旧摘要与较早原文，保留约一半近期原文；摘要与进度持久落库后再继续回复。记忆提取按 `[memory].extract_every_chars=10000` 独立累计完整用户/助手原文字数，每满 1 万字提交一次批量任务；进度跨重启保留，始终从原始用户消息取证，遵守原有筛选规则。压缩失败不推进游标或删除历史。对话页底部显示系统、历史、摘要、记忆和工具的上下文估算，展开可查看详细分布、最近请求的 API 实际用量、会话累计（按对话/压缩/记忆提取区分）、摘要与压缩次数；Agent 页集中显示预算与提取周期。对话页显示 QQ/网页最近 200 条记录的来源、时间和完成状态。本地使用内置 `o200k_base` 分词器计数，包含系统提示、记忆、历史、工具和回复预留后决定是否压缩；所有分项估算明确标记；API 未提供的字段显示“未提供”，不会补造推理。模型返回的思考文本独立折叠展示并支持流式更新，刷新后仍可查看；输出消耗已含推理，不重复累计，也不直接作为下一轮上下文。`history_turns` 仅保留旧配置兼容，不再触发压缩或截断历史。旧 `[llm].context_chars` 和旧字符环境变量不再生效，缺省升级到 10 万 token；旧字符配置中的默认 16 轮升级到 1000，显式新 token 配置和环境覆盖优先。记录每 3 秒自动更新，保留正在输入的草稿；刷新时若上一轮仍在生成，会等待并自动显示完成结果。

## 性格与重置

WebUI 修改的性格保存在 SQLite 的 metadata 中，优先于配置文件的 `[persona]`，HTTP 与 QQ 对话均使用同一份即时设置。各项合计最多 6000 字，名字最多 80 字，且须符合当前上下文容量。点击“填入配置文件默认值”只填充表单，保存后生效。

“输出格式 Prompt”对应 `[persona].output_format`，可以填写回复结构、是否使用 Markdown 或 JSON 等格式要求，例如“先给一句结论，再用纯文本分段说明”。非空时每轮加入系统提示词，留空表示不额外约束输出格式；这是对模型的提示，不做响应格式的强制校验。该字段与其他性格项一起保存、恢复默认值并计算字符预算；旧配置和旧版保存的性格无需补填。

重置会在同一事务中删除所有会话、原始对话、长期记忆、来源、修订记录、遗忘屏障、后台任务、图片、工具调用记录和 QQ 收发队列。浏览器上下文会关闭，离线时保留待清理标记，恢复后先执行清理。保留服务连接配置、API Token、WebUI 密码、QQ 账号归属及模型选型缓存；QQ 下一条新消息会重新建立会话。默认保留当前性格，勾选后可恢复文件默认值。界面要求输入 `RESET`，取消不会修改数据。

服务端先暂停新任务领取，最多等待当前请求、生成与后台任务 3 秒；仍繁忙时返回 409 `agent_busy`，不清空任何数据。成功后旧记忆立即停止召回，Neo4j 清理通过持久化标记交给后台执行；图谱离线或服务重启后继续重试，清理完成前不投影新记忆。SQLite 逻辑记录已删除，但此功能不代表磁盘安全擦除，也不会删除已有备份或外部服务的数据。

## 编辑、删除与索引一致性

新增和编辑先在一个 SQLite 事务内提交内容、修订快照和持久化投影任务。模型离线时仍可保存，页面显示“等待后台更新”；工作队列负责生成文档向量并更新 Neo4j 全文、向量和实体关系。内容没有变化时复用原向量。成功同步后记录 `indexed_revision`，不会用旧任务把新版本误标为已同步。

编辑要求 `expected_revision`。服务端在持有 SQLite 写事务时检查版本；另一个页面已经修改或删除时返回 409，不覆盖现有内容。人工编辑保留记忆 ID 和原始来源，同时标记 `origin=manual`。自动提取引发的属性替代继续沿用原有记忆版本链。

删除采用带 `deleted_at` 的逻辑墓碑，不物理擦除原始对话或修订历史。删除后默认列表和召回立即排除该记录，后台移除 Neo4j 节点与无关联的实体。索引是最终一致的，检索出口始终以 SQLite 当前状态为准，Reranker 返回后会再次核对版本，避免交付检索期间已修改或删除的内容。

人工编辑/删除还会记录属性键和所有历史内容的消息序号屏障，抑制已有来源消息的旧提取任务。向量写回采用“ID + revision + active + 未删除”的条件更新；删除后重试旧任务、重新生成向量或执行 reindex 都不能恢复原记录。不存在私有记忆列表或检索结果的进程缓存；模型服务缓存仅由输入文本决定编码结果，无权更改记忆状态。之后用户在新消息中重新明确提供事实，或主动手动新增同样内容，可以建立新记录。

## API

除密码登录外，浏览器接口使用登录 Cookie；终端和集成仍使用 `Authorization: Bearer <MNEME_API_TOKEN>`。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/v1/console/auth` | 恢复并验证登录状态，不触发外部模型探测 |
| GET | `/v1/console/messages` | 全渠道最近 200 条记录，包含来源、完成状态、关联图片与工具调用 |
| GET | `/v1/console/stats` | 仪表盘统计、分类、7 日活动和任务数 |
| GET | `/v1/memories?q=成都&kind=fact&status=active&sort=updated&limit=15&offset=0` | 分页浏览；返回 memories、total、limit、offset |
| POST | `/v1/memories` | 手动新增记忆，返回 201 与 index_update=queued |
| PUT | `/v1/memories/{id}` | 编辑有效记录，要求 expected_revision |
| GET | `/v1/memories/{id}` | 当前记录、sources、versions |
| DELETE | `/v1/memories/{id}?expected_revision=2` | 删除并建立保护屏障；旧客户端可省略版本参数 |
| POST | `/v1/console/retrieval` | `{"query":"我住在哪里？"}`，返回 results、trace、degraded |
| GET | `/v1/console/tools` | 工具服务健康、沙箱、容量和调用性能 |
| GET | `/v1/console/tools/calls` | 最近 100 次调用，可用 session_id 过滤 |
| POST | `/v1/console/tools/close` | `{"session_id":"UUID"}`，释放浏览器上下文 |
| GET | `/v1/console/system` | 服务状态和性能；探测结果缓存 10 秒 |
| POST | `/v1/console/login` | `{"password":"…"}`，设置登录 Cookie，返回 authenticated 和 expires_in；不要求 Bearer |
| POST | `/v1/console/logout` | 撤销当前密码会话 |
| GET | `/v1/console/settings` | persona、default_persona、password_configured、graph_cleanup_pending；不返回密码哈希或密钥 |
| PUT | `/v1/console/persona` | `{"name":"…","description":"…","style":"…","boundaries":"…","output_format":"…"}`；output_format 可省略，默认为空 |
| PUT | `/v1/console/password` | `{"current_password":"…","new_password":"…"}`；new_password 必须为 10–128 字符；API Token 可免当前密码用于恢复 |
| POST | `/v1/admin/reset` | `{"confirmation":"RESET","reset_persona":false}`，返回删除条数和图谱清理排队状态 |

新增/编辑请求示例：

```json
{
  "content": "用户喜欢不加糖的拿铁。",
  "kind": "preference",
  "importance": 0.8,
  "confidence": 1.0,
  "entities": ["咖啡", "拿铁"],
  "valid_until": null
}
```

编辑时额外传入 `expected_revision`，例如 1。`valid_until` 为 Unix 秒；页面输入按浏览器本地时区转换。内容上限 500 个字符；分类为 fact/preference/event/relationship/emotion，重复有效内容返回 409。status 支持 all/active/invalid/expired/forgotten/superseded/deleted；sort 支持 updated/created/oldest/importance/confidence。检索调试的 query 最多 4000 字符。

LLM 状态通过配置提供商的 `/models` 目录接口检查，不发起生成请求，也不代表完整生成质量已验收；404/405/501 或非标准目录响应显示“待确认”。Embedding 和 Reranker 共用本地模型服务，状态来自它的加载完成标记；各自推理表现来自实际调用统计。耗时反映本进程观察到的请求时间，重启后清零。

## 升级与测试

先备份 SQLite，再拉取新镜像并更新、重启服务。可先设置 `MNEME_IMAGE_TAG=sha-完整提交SHA` 固定版本：

```bash
bash scripts/pull.sh
bash scripts/install.sh
systemctl --user restart mneme.target
```

`0005_continuity.sql` 增加滚动摘要、迁移别名、提取完成标记和持久登录会话表。后续 `0006_global_context.sql` 保留历史消息渠道并增加轮次索引；启动时将 QQ、网页和 API 原子合并到全局对话。合并多份旧上下文时从保留的原文重建滚动摘要，避免不同游标跳过历史。

新增工具的 `0004_tools.sql` 迁移只增加图片与工具表，不改写旧迁移；工具接口和浏览器验证见 [tools.md](tools.md)。

早期 `0002_console.sql` 迁移在启动时自动执行，保留旧记忆及来源，为已有行建立初始修订快照，并排队同步旧投影及其完成标记。未改写 `0001_initial.sql`。无需重新下载模型或创建新数据卷。因前端编译进二进制，修改 web/ 后也需要重新编译 API。

常规和真实服务测试：

```bash
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
node --check web/app.js
bash scripts/smoke-services.sh
python3 scripts/smoke-pod.py
```

浏览器测试仅为开发工具，不是运行时依赖。使用现有 Chromium，在一个终端启动专用临时 SQLite/上游替身：

```bash
MNEME_QQ_HISTORY_UI_TEST=1 cargo run --locked --example console_fixture
```

另一个终端运行（不接触生产 Token 或生产数据库）：

```bash
npm install --prefix /tmp/mneme-console-qa --no-save --no-package-lock playwright-core@1.56.1
MNEME_QQ_HISTORY_UI_TEST=1 NODE_PATH=/tmp/mneme-console-qa/node_modules node scripts/console-browser-test.cjs
```

测试验证桌面、390px 移动端、真实 CRUD/历史修订、检索过程、系统状态、HTML 转义、退出、刷新和新标签页的登录持续性，以及持续对话与压缩、性格保存、密码登录和轮换、取消和确认重置，并将截图保存到忽略的 `artifacts/`。测试结束后在第一个终端按 Ctrl+C 关闭临时服务。真实数据和外部付费 LLM 不参与该浏览器测试。
