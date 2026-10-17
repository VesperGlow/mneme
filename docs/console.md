# Mneme Console

Console 内置在 Mneme 的 Axum 服务中。启动现有服务后打开 **http://127.0.0.1:8080/console/**，使用现有 `MNEME_API_TOKEN` 登录即可。无需 Node.js、前端构建步骤、额外端口或独立容器。HTML、CSS、JavaScript 和图标在编译时嵌入 Rust 二进制，不从 CDN 加载脚本、字体或其他资源。

## 功能

- **概览**：未删除记忆的总数、有效/失效数量、过去 7×24 小时新增量、后台任务数、按类型分类、最近 7 个 UTC 自然日的新增图和近期记忆。有效指状态为 active 且尚未过期；最终召回还会考虑强度与相关性。
- **记忆库**：按内容、关联实体或属性键搜索；按类型和有效/失效/删除状态筛选；按创建、更新时间、重要性、可信度排序；服务器分页。新增与编辑支持内容、类型、重要性、可信度、有效期和最多 8 个关联实体。
- **详情**：原始内容、人工/自动来源、重要性、可信度、时间、实体、索引同步状态、最近 100 条来源消息及最近 100 次相关修订。对话来源可展开查看原始用户消息。旧版本通过相同属性键串联；人工编辑之前的证据仅用于追溯，不自动成为新内容的证明。
- **检索实验室**：使用与实时对话完全相同的管线，展示 Neo4j 向量、CJK 全文、实体匹配、图谱扩展、近期 SQLite 补充候选、RRF 分数、Reranker 分数、最终得分和筛除原因。各通道原始分数不可直接比较；精排服务异常时显示回退状态。删除的内容不会在调试候选中回显。
- **系统状态**：LLM、Neo4j、Embedding、Reranker 的连通/加载状态，探测耗时，当前进程已完成请求的平均耗时、次数、失败数，运行时间、SQLite 大小、任务队列。可重新排队失败任务或安排索引重建。
- **Agent 设置**：编辑名字、性格描述、交流方式和边界，保存后下一轮对话生效；设置、修改或移除独立 WebUI 登录密码；输入 `RESET` 确认重置 Agent，可同时恢复配置文件中的默认性格。

桌面端使用侧边导航和详情抽屉，移动端使用底部导航与自适应列表。支持键盘焦点、原生对话框、Escape 关闭以及 `/` 快速进入搜索。

## 身份与访问

公开页面只包含登录界面和静态资源，所有私人数据都由受 Bearer Token 保护的 API 加载。Console 不使用 Cookie、localStorage、sessionStorage 或 URL 查询参数保存凭证。Token 仅在当前页面的 JavaScript 内存中存在；刷新、离开页面、退出或闲置超过 30 分钟会清除凭证和私人页面内容。API 使用 `no-store`，页面禁止被 iframe 嵌入，CSP 仅允许本站脚本、样式和连接。跨站浏览器请求直接拒绝。

默认原生服务监听 `127.0.0.1:8080`；现有 Quadlet 仅将 Pod 端口发布到宿主机的 `127.0.0.1`。不需要为了 Console 修改监听或发布设置。远程使用可沿用 SSH 转发：

```bash
ssh -L 8080:127.0.0.1:8080 user@server
```

Token 仍是原有单用户管理凭证，授权者可以访问全部对话和记忆。Console 不引入多用户角色或额外账户系统。

首次使用 API Token 登录后，可在 Agent 设置中设置 10–128 字符的 WebUI 密码。之后在登录页选择“WebUI 密码”，服务端验证后签发随机会话凭证，仅在页面内存保存。密码使用随机盐和 PBKDF2-HMAC-SHA256（600,000 次）哈希保存到 SQLite，不保存明文。会话闲置 30 分钟过期，退出会撤销当前会话；服务重启或密码变更会撤销所有密码会话。登录失败会短暂限速。API Token 始终保留原有权限，可用于找回或移除忘记的密码；通过密码会话修改密码时还需验证当前密码。

## 性格与重置

WebUI 修改的性格保存在 SQLite 的 metadata 中，优先于配置文件的 `[persona]`，HTTP 与 QQ 对话均使用同一份即时设置。各项合计最多 6000 字，名字最多 80 字，且须符合当前上下文容量。点击“填入配置文件默认值”只填充表单，保存后生效。

重置会在同一事务中删除所有会话、原始对话、长期记忆、来源、修订记录、遗忘屏障、后台任务和 QQ 收发队列。保留服务连接配置、API Token、WebUI 密码、QQ 账号归属及模型选型缓存；QQ 下一条新消息会重新建立会话。默认保留当前性格，勾选后可恢复文件默认值。界面要求输入 `RESET`，取消不会修改数据。

服务端先暂停新任务领取，最多等待当前请求、生成与后台任务 3 秒；仍繁忙时返回 409 `agent_busy`，不清空任何数据。成功后旧记忆立即停止召回，Neo4j 清理通过持久化标记交给后台执行；图谱离线或服务重启后继续重试，清理完成前不投影新记忆。SQLite 逻辑记录已删除，但此功能不代表磁盘安全擦除，也不会删除已有备份或外部服务的数据。

## 编辑、删除与索引一致性

新增和编辑先在一个 SQLite 事务内提交内容、修订快照和持久化投影任务。模型离线时仍可保存，页面显示“等待后台更新”；工作队列负责生成文档向量并更新 Neo4j 全文、向量和实体关系。内容没有变化时复用原向量。成功同步后记录 `indexed_revision`，不会用旧任务把新版本误标为已同步。

编辑要求 `expected_revision`。服务端在持有 SQLite 写事务时检查版本；另一个页面已经修改或删除时返回 409，不覆盖现有内容。人工编辑保留记忆 ID 和原始来源，同时标记 `origin=manual`。自动提取引发的属性替代继续沿用原有记忆版本链。

删除采用带 `deleted_at` 的逻辑墓碑，不物理擦除原始对话或修订历史。删除后默认列表和召回立即排除该记录，后台移除 Neo4j 节点与无关联的实体。索引是最终一致的，检索出口始终以 SQLite 当前状态为准，Reranker 返回后会再次核对版本，避免交付检索期间已修改或删除的内容。

人工编辑/删除还会记录属性键和所有历史内容的消息序号屏障，抑制已有来源消息的旧提取任务。向量写回采用“ID + revision + active + 未删除”的条件更新；删除后重试旧任务、重新生成向量或执行 reindex 都不能恢复原记录。不存在私有记忆列表或检索结果的进程缓存；模型服务缓存仅由输入文本决定编码结果，无权更改记忆状态。之后用户在新消息中重新明确提供事实，或主动手动新增同样内容，可以建立新记录。

## API

除密码登录外，所有接口使用 `Authorization: Bearer <MNEME_API_TOKEN 或临时会话凭证>`。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/v1/console/auth` | 验证 Token，不触发外部模型探测 |
| GET | `/v1/console/stats` | 仪表盘统计、分类、7 日活动和任务数 |
| GET | `/v1/memories?q=成都&kind=fact&status=active&sort=updated&limit=15&offset=0` | 分页浏览；返回 memories、total、limit、offset |
| POST | `/v1/memories` | 手动新增记忆，返回 201 与 index_update=queued |
| PUT | `/v1/memories/{id}` | 编辑有效记录，要求 expected_revision |
| GET | `/v1/memories/{id}` | 当前记录、sources、versions |
| DELETE | `/v1/memories/{id}?expected_revision=2` | 删除并建立保护屏障；旧客户端可省略版本参数 |
| POST | `/v1/console/retrieval` | `{"query":"我住在哪里？"}`，返回 results、trace、degraded |
| GET | `/v1/console/system` | 服务状态和性能；探测结果缓存 10 秒 |
| POST | `/v1/console/login` | `{"password":"…"}`，返回 token 和 expires_in；不要求 Bearer |
| POST | `/v1/console/logout` | 撤销当前密码会话 |
| GET | `/v1/console/settings` | persona、default_persona、password_configured、graph_cleanup_pending；不返回密码哈希或密钥 |
| PUT | `/v1/console/persona` | `{"name":"…","description":"…","style":"…","boundaries":"…"}` |
| PUT | `/v1/console/password` | `{"current_password":"…","new_password":"…"}`；new_password 为空表示移除，API Token 可免当前密码 |
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

新增 `0002_console.sql` 迁移在启动时自动执行，保留旧记忆及来源，为已有行建立初始修订快照，并排队同步旧投影及其完成标记。未改写 `0001_initial.sql`。无需重新下载模型或创建新数据卷。因前端编译进二进制，修改 web/ 后也需要重新编译 API。

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
cargo run --locked --example console_fixture
```

另一个终端运行（不接触生产 Token 或生产数据库）：

```bash
npm install --prefix /tmp/mneme-console-qa --no-save --no-package-lock playwright-core@1.56.1
NODE_PATH=/tmp/mneme-console-qa/node_modules node scripts/console-browser-test.cjs
```

测试验证桌面、390px 移动端、真实 CRUD/历史修订、检索过程、系统状态、HTML 转义、退出/刷新与闲置锁定，以及性格保存、密码登录/移除、取消和确认重置，并将截图保存到忽略的 `artifacts/`。测试结束后在第一个终端按 Ctrl+C 关闭临时服务。真实数据和外部付费 LLM 不参与该浏览器测试。
