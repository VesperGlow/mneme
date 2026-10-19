# QQ 单聊接入

mneme 按 [官方 BotGo](https://github.com/tencent-connect/botgo) 的签名和 Webhook 协议直接用 Rust 实现适配，不增加 Go 进程或容器。官方已经停止维护旧 WebSocket 链路，因此使用 HTTPS 回调。

## 填写配置并启动

先按 README 安装 Rootless Podman、启用用户 systemd，然后复制 `.env.example` 为 `.env`，填写四项：

```dotenv
MNEME_LLM_API_KEY=你的外部模型服务密钥
MNEME_LLM_BASE_URL=https://你的模型服务域名/v1
MNEME_QQ_APP_ID=你的数字AppID
MNEME_QQ_APP_SECRET=你的AppSecret
```

```bash
bash scripts/start.sh
```

脚本准备配置、拉取 amd64 镜像并启动同一个 Pod 的三个服务。Console Token 和 Neo4j 密码留空即可；自动生成并写回权限为 0600 的 `.env`，重装时保留。保存好 `.env` 和 `~/.config/mneme/`；已有 Neo4j 卷的认证密码不会因修改配置而自动改变。

`MNEME_LLM_MODEL` 未填时，首次请求 `/models`，按提供商返回顺序选择第一个排除已知 Embedding、Reranker、图像、语音等专用类型的模型，并按服务地址缓存在 SQLite，重启不会重新选型。这个规则无法判断所有模型的能力、价格和参数；建议明确指定常用聊天模型。目录接口不受支持时，须填写模型名。URL 可以是 `/v1` 基础地址、服务根地址或完整 `/chat/completions` 地址，安装器会规范化；不要把密钥放进 URL。

QQ 两项同时留空时关闭适配，仍可使用原有 HTTP API 和 Console。

## 配置 QQ 开放平台

AppID/AppSecret 不能代替平台上的以下设置：

1. 创建机器人；未上线时把自己的 QQ 加入沙箱成员。
2. 配置服务器固定公网出口 IP 白名单。
3. 回调填写 `https://你的公网域名/qqbot`，监听 `C2C_MESSAGE_CREATE`；平台会自动发起签名挑战验证。
4. 默认正式 OpenAPI；若平台要求沙箱 API，设置 `MNEME_QQ_SANDBOX=true` 后重新安装、重启。

服务端只发布本机 `127.0.0.1:8081`，该端口仅接受 `POST /qqbot`，没有 Console 或管理 API。请用已有 HTTPS 反向代理仅转发此路径；[Nginx 示例](../deploy/nginx/qqbot.conf.example) 放入已有证书的 HTTPS server 块。管理端口 8080 继续仅供本机或 SSH 转发访问。

官方参考：[Webhook 与验签](https://bot.q.qq.com/wiki/develop/api-v2/dev-prepare/interface-framework/event-emit.html)、[Access Token](https://bot.q.qq.com/wiki/develop/api-v2/dev-prepare/access-token.html)、[单聊发送消息](https://bot.q.qq.com/wiki/develop/api-v2/autogen/api/v2_users_user_openid_messages.post.html)。

## 直接开始私聊

回调验证成功后，用自己的 QQ 给机器人发送文字即可。第一条私聊会在 SQLite 事务中自动建立个人账号和会话，并进入正常回复流程；不需要生成绑定码、发送 `/绑定` 或先登录 Console。

Mneme 作为个人机器人持续使用第一个私聊账号；已有版本保存的个人账号和会话会继续沿用，其他账号、群聊和频道事件不会进入私人记忆。更新镜像无需清空数据库。旧版未绑定时忽略的消息没有进入队列，更新后重新发送一次即可。

QQ、网页和 API 共享同一个持久上下文，上下文预算默认 100000 tokens（本地分词估算），累计新增 10000 字后批量提取长期记忆。`/新会话` 仅提示持续对话模式，不再清空或分叉上下文；`/帮助` 查看说明。支持文字、纯图片和图文消息，图片由现有多模态 LLM 理解。完整回复与引用图片先落库，再投递 QQ；QQ 不展示尚未提交的流式片段。

Console 的“Agent → QQ 机器人”显示个人会话和投递队列，用于查看运行情况。个人账号、会话、内容、回复及分段投递进度均保存在 SQLite，重启后恢复。更换 AppID 会使用新的 QQ 会话映射，个人记忆仍是单用户设计。

## 图片与自定义表情包

启用 `MNEME_TOOLS_ENABLED=true`，确保工具服务运行，并配置支持 `image_url` 的多模态聊天模型。QQ 私聊接收 PNG、JPEG、WebP、GIF（每条最多 4 张，每张不超过 8 MiB），纯图片消息也会进入对话。Webhook 在保存附件地址后立即 ACK，由队列下载、验证并转为 JPEG，再与文字一起送入模型；GIF 看图使用首帧。下载逐跳验证公网地址并固定解析 IP，禁止内部网络、携带凭证的地址和超限响应。无法读取时会明确提示重新发送，不声称看到了图片。

在 **Agent → 自定义表情包** 上传自己的素材、填写使用场景，Agent 即可在 QQ 与网页回复中共用。表情动图保留原格式与动画；QQ 最终展示效果由平台决定。Agent 还可以按用户要求发送当前对话中已上传的图片或工具截图。通过 QQ 的 `POST /v2/users/{openid}/files` 上传 Base64 数据，再用 `msg_type=7` 与 `media.file_info` 发送；无需公开 Console 图片地址。协议参考 [腾讯官方 SDK 的媒体说明](https://github.com/tencent-connect/qqbot-nodejs/blob/main/USAGE.md#10-媒体上传与发送)。

文字最多四段，文字加图片合计最多五条。图片失败后复用已保存的回复与媒体，从原 `msg_seq` 继续投递；不会重复生成回复或重发已经确认成功的文字。重启也保留这一进度。未收到平台成功响应的请求仍依赖 QQ 按 `msg_id + msg_seq` 去重。

## 投递与验证边界

- 校验 AppID、Ed25519 签名和 5 分钟时间窗；入库成功才返回成功 ACK。按 AppID + 消息 ID 去重；入库失败或队列已满返回失败 ACK，请求平台重投。
- Token 在服务端缓存，使用前自动检查有效期、到期前刷新，401 后重新获取；不写入数据库或日志。AppSecret 不进入前端。
- 消息按接收顺序处理；网络重试复用已落库的回复，不重复调用 LLM。最多四段，使用稳定的 `msg_id + msg_seq`；超过 6000 字时 QQ 提示截断，完整内容仍在原始对话中。
- 平台已接收但 HTTP 响应丢失时，无法保证跨系统严格恰好一次投递；重试保持序号，依赖 QQ 去重。
- 按 [腾讯当前单聊文档](https://github.com/tencent-connect/bot-docs/blob/main/docs/develop/api-v2/server-inter/message/send-receive/send.md)，C2C 被动回复有效期为 60 分钟；队列在原消息 3590 秒后过期，预留 10 秒给投递。该窗口支持较长的浏览任务，排队时间也计入有效期；过期、永久拒绝或多次失败进入 dead 计数，不改发主动消息。Webhook 签名的新鲜度校验仍为 5 分钟，未放宽。
- Console 编辑或删除记忆同时记录 QQ 接收队列水位；操作前已排队的旧输入不能恢复该记忆，之后明确重新告知的事实仍可正常记忆。

测试覆盖官方签名向量、签名挑战、鉴权与端口隔离、首条私聊自动回复、去重、Token 刷新、投递失败后恢复、重启后的跨会话记忆及遗忘屏障，以及纯图片接收、视觉输入和媒体投递失败后的重启恢复。测试使用本地 QQ HTTP 替身，不向真实 QQ 账号发送消息。真实 AppID/AppSecret 和公网回调未提供，因此尚未完成 QQ 线上联调。
