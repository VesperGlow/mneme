# 运维

## 观察与故障恢复

`GET /health/ready` 的 503 表示模型或图谱不可用，进程不会因此退出。外部 LLM 不在 readiness 中执行付费探测。日志默认不记录原始消息、完整上游错误体或密钥；任务失败使用稳定错误码，可结合失败种类、次数和本地服务日志定位。

检查：

```bash
systemctl --user status mneme.target mneme-api mneme-models mneme-neo4j mneme-tools
journalctl --user -u mneme-api -u mneme-models -u mneme-neo4j -u mneme-tools --since '15 min ago'
podman stats --no-stream mneme-api mneme-models mneme-neo4j mneme-tools
curl -sS -H "Authorization: Bearer $MNEME_API_TOKEN" http://127.0.0.1:8080/health/ready
```

提供商必须支持普通 Chat Completions 和 SSE 的 `[DONE]` 结束帧，流式终止原因支持 stop/length；提取请求要求完整 stop 响应和 JSON 内容，但不依赖供应商特有的 structured output 参数。`length` 回复会保存为完整已结束轮，用户可能看到因 max_tokens 截短的回答。提取 JSON 失败会退避重试，最终进入死信；如果供应商持续返回解释文本而非 JSON，需要调整模型或提取提示词。

QQ 只显示“收到消息”或持续投递失败时，在项目目录运行：

```bash
python3 scripts/diagnose.py --chat
```

诊断直接读取运行中 API 容器的配置，通过现有模型容器在同一个 Pod 网络中检查 QQ Access Token、LLM 模型目录及一次最多 64 token 的流式生成。配置为自动选型时优先读取正在运行的 API 的实际模型。脚本仅输出状态码、固定错误类别及流式完成标记，密钥通过进程标准输入传递，不打印 Token、完整响应、消息或带密钥的 URL，也不跟随重定向。只运行网络和凭证检查可省略 `--chat`。容器名有改动时使用 `--api-container` 和 `--probe-container` 指定；需要 Python 3.11+ 以及两个容器均处于运行状态。

Python 检查通过但 Rust 仍在 `qq.token` 超时时，直接使用运行容器内的真实 Rust 客户端检查：

```bash
podman exec mneme-api mneme --check-qq
```

此命令读取容器现有配置，输出 `qq_token`、成功连接方式 `transport` 和经过筛选的 `failure`，不打开数据库或监听新端口。QQ 默认连接遇到网络或握手错误后，会自动尝试 IPv4、HTTP/1.1、TLS 1.2 的兼容连接，连接超时 10 秒、请求总超时 20 秒；成功后获取 Token 和发送消息共用该连接方式。保留证书与域名验证，继续遵循已配置的代理，不写死服务 IP。HTTP 鉴权拒绝和 API 业务错误不会触发切换。Python 的成功结果用于对比网络和协议，不能代替 Rust 检查或真实 QQ 消息发送验收。

新增日志中的 `stage=llm.chat` 表示生成请求，`stage=qq.token` 表示 QQ 凭证获取，`stage=qq.send` 表示消息发送。`category=timeout/dns/tls/connection_refused/network_unreachable` 定位网络层问题；`category=http` 同时附带 HTTP 状态码；`incomplete_stream` 表示提供商未正常结束流。`QQ reply generation started` 与 `QQ reply delivery started` 分别标记生成和发送阶段。`/帮助` 不调用 LLM，但仍按 QQ 接收队列顺序处理，不能据此跳过正在等待的前一条消息。

修复依赖或配置后：

```bash
curl -sS -X POST -H "Authorization: Bearer $MNEME_API_TOKEN" \
  http://127.0.0.1:8080/v1/admin/jobs/retry
```

若图谱被清空或恢复到较旧的快照，先确保 Neo4j 启动，再 POST `/v1/admin/reindex`。后台会重新创建索引并从 SQLite 向量和实体重建；无需再次调用外部 LLM。

## 内置模型与构建下载

浏览器工具镜像为 `ghcr.io/vesperglow/mneme-tools:latest`，加入已有 `mneme.pod`，无需发布新端口。工具状态和错误可在 Console 的“工具”页查看；单独重启可执行 `systemctl --user restart mneme-tools`。该服务使用用户命名空间沙箱，宿主机必须允许非特权用户命名空间；启动失败时检查内核/AppArmor 与 seccomp 日志，不要添加 `--no-sandbox` 或宿主机权限。搜索使用 Tavily，在 `.env` 设置 `MNEME_TAVILY_API_KEY`（多个 key 用 `/` 分隔）后重新安装并重启；浏览网址继续使用 Chromium。Tavily 凭证无效、额度耗尽或限流，以及网页验证码、目标站限流、页面不可提取均返回明确错误。安装、接口、资源与共享网络限制见 [tools.md](tools.md)。

发布的模型镜像内置两个模型以及分词器、配置、GTE 自定义代码，文件约 1.8 GB，默认严格离线加载。首次拉取镜像后，服务启动和重启都不访问外部模型下载站。GitHub 构建流程在 `--network none`、只读根文件系统下实际加载两个 CPU 模型，并验证 1024 维归一化向量和中文精排，通过后才发布。

内置文件在 `/opt/mneme-models/hub`，通过 `MNEME_BUNDLED_MODEL_CACHE` 定位；现有 `/models` 卷不会遮挡它们，旧缓存及断点文件原样保留。默认 `HF_HUB_OFFLINE=1`，启动逐一校验 SHA-256 后从镜像只读路径加载；自定义代码的临时缓存通过 `HF_MODULES_CACHE=/tmp/mneme-modules` 写入已有 `/tmp` tmpfs。缓存缺失或校验错误时明确启动失败，不会悄悄在部署机器重新联网下载。

更新旧部署不需要调整 `.env` 或 Quadlet：

```bash
podman pull ghcr.io/vesperglow/mneme-models:latest
systemctl --user restart mneme-models.service
podman logs -f mneme-models
```

日志出现 `bundled model files verified` 后开始 CPU 加载，最终出现 `CPU embedding and reranker ready` 和 `Application startup complete`。

模型在镜像构建阶段下载，`services/models/artifacts.json` 固定共 13 个文件的版本、大小、SHA-256 和魔搭提交映射。默认先 Hugging Face，失败后魔搭。每个源使用独立 `.part` 文件，优先续传已有断点的源；按 8 MiB 分段请求，断流时逐步缩小到 1 MiB 并原地续传，单段最多尝试 5 次、退避 1/2/4/8 秒。每次连接超时 5 秒、读取超时 15 秒。认证、范围和校验错误直接换源；日志区分断流、连接、TLS 等固定类别，不打印签名 URL。完整大小和 SHA-256 校验通过后才刷盘并原子发布。

自建镜像可添加 `--build-arg MNEME_MODEL_SOURCES=modelscope,huggingface` 调整构建下载源，或仅填 `modelscope`。不要把 `HF_ENDPOINT` 设置为魔搭地址，两者的下载 URL/API 格式不同。直接在 Python 环境运行时，未设置 `MNEME_BUNDLED_MODEL_CACHE` 才会使用原有的下载及持久卷缓存路径；`HF_HUB_OFFLINE=1` 可要求仅使用已有缓存。

## 自动备份与检查

完整 `scripts/install.sh` 会安装并启用 `mneme-backup.timer`。定时器每天按宿主机时区 03:30 后随机延迟最多 30 分钟运行，错过时补做。单服务更新保持既有定时器；从旧版升级启用它需执行一次完整安装。检查和手动触发：

```bash
systemctl --user list-timers mneme-backup.timer
systemctl --user start mneme-backup.service
journalctl --user -u mneme-backup.service -n 30
```

快照位于 `~/.local/state/mneme/backups/mneme-时间-随机串/`，包含 `mneme.sqlite3`、`config/` 和 SHA-256 清单 `manifest.json`。配置备份包含密钥，目录权限 0700、文件 0600。默认保留最近 14 份成功快照，只清理本脚本创建并带清单的目录；失败时不删除此前备份。符号链接配置会明确报错，不沿链接读取其他文件。

每次备份先通过 SQLite 在线备份 API 保存已提交 WAL，再恢复到独立临时数据库检查完整性、外键和所有表行数，成功后才发布快照。它验证 SQLite 可恢复，不替代整套 Pod/图谱重建的验收。恢复快照时，使用下文步骤指定其中的 `mneme.sqlite3`；需要恢复部署配置时另行核对 `config/`，不会自动覆盖当前配置。

本地原生部署也可运行：

```bash
python3 scripts/backup-scheduled.py --database data/mneme.sqlite3 \
  --config ~/.config/mneme --destination ~/.local/state/mneme/backups --retain 14
```

自定义部署布局可重复传入 `--config-file /path/to/file`，明确选择 TOML、环境文件和 Quadlet 文件，替代整个配置目录；文件名须互不重复。

停止后续自动备份可执行 `systemctl --user disable --now mneme-backup.timer`，既有快照保留。

## 从 SQLite 备份恢复

先用 `scripts/backup.py` 生成并检查备份。恢复时停止整个 target，确保没有 SQLite 写者，保留现有数据以便回滚。

```bash
systemctl --user stop mneme.target
mneme_volume_path=$(podman volume inspect mneme-data --format '{{.Mountpoint}}')
podman unshare mkdir -p "$mneme_volume_path/before-restore"
podman unshare python3 - "$mneme_volume_path" backups/mneme-YYYYMMDDTHHMMSSZ.sqlite3 <<'PY'
import os, shutil, sqlite3, sys
from pathlib import Path
volume, backup = map(Path, sys.argv[1:])
with sqlite3.connect(f'{backup.resolve().as_uri()}?mode=ro', uri=True) as conn:
    assert conn.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
archive = volume / 'before-restore'
os.chmod(archive, 0o700)
for suffix in ('', '-wal', '-shm'):
    if (archive / ('mneme.sqlite3' + suffix)).exists():
        raise SystemExit('before-restore 中已有旧文件，请先归档后再恢复。')
for suffix in ('', '-wal', '-shm'):
    old = volume / ('mneme.sqlite3' + suffix)
    if old.exists():
        target = archive / old.name
        old.rename(target)
shutil.copyfile(backup, volume / 'mneme.sqlite3')
os.chmod(volume / 'mneme.sqlite3', 0o600)
PY
systemctl --user start mneme.target
curl -sS -X POST -H "Authorization: Bearer $MNEME_API_TOKEN" \
  http://127.0.0.1:8080/v1/admin/reindex
```

图谱可以存在备份之后的陈旧节点，检索出口会过滤不在 SQLite 的节点。要完全恢复到备份时点，应在专用 Neo4j 数据库中清空现有 `Memory`/`Entity` 投影后重建，或使用全新的 Neo4j 数据卷。不要在共享给其他应用的图数据库上执行清空操作。重新创建 Neo4j 卷时会按 `neo4j.env` 初始化认证。

## 密钥、升级与离线运行

- `app.env`、`tools.env` 和 `neo4j.env` 权限为 0600，配置目录为 0700；不要把 `.env` 或备份纳入 Git。具备本机同用户 Podman 权限的主体能够读取容器环境，按操作系统用户边界管理访问。
- 修改 API Token 或外部 LLM Key 后，重新执行安装脚本并重启 API。修改 `.env` 中的 Neo4j 密码不会修改已有数据卷里的认证信息，需要通过 Neo4j 管理命令先修改数据库密码，再同步环境配置。
- WebUI 密码可在 Console 的设置页修改；忘记时在本机终端使用 API Token 调用 `PUT /v1/console/password` 设置新密码；WebUI 不接受 API Token 登录，也不支持移除密码。无需修改服务环境或重新安装。WebUI 性格覆盖、密码哈希、登录会话及对话摘要都随 SQLite 备份恢复；API Token 仍来自部署环境。
- 预构建镜像仅支持 `linux/amd64`，默认 `latest` 跟随 main。生产部署建议在拉取和安装前设置 `MNEME_IMAGE_TAG=sha-完整提交SHA`，固定 API、模型与工具镜像到同一提交；Neo4j tag、Rust `Cargo.lock`、Python constraints lock、模型权重及自定义代码 revision 均固定。升级依赖前运行全部常规与真实模型测试，备份 SQLite，再拉取或构建并重启。已发布迁移不可改写，新增编号迁移；数据库结构升级后回退二进制需要匹配备份。
- 发布的模型镜像默认已设置 `HF_HUB_OFFLINE=1`，从镜像内置的固定模型离线启动；外部 LLM 仍需要网络。
- 若替换 Embedding 模型/维度，必须重新计算全部持久化向量并重建 Neo4j 索引，不能仅修改模型服务名称。v1 固定为 Qwen3 1024 维，不暴露不安全的热切换。

CPU 延迟依赖机器、文本长度和队列。默认 512 token、Embedding batch=2、Reranker batch=4 优先控制峰值；大量旧任务时先检查提供商配额和模型队列，再调整并发，避免在 4 核机器上让多个模型进程争抢 CPU。


## 按服务更新

日常不必重启整个 Pod。运行 `bash scripts/update.sh api tools` 只拉取和重启 API、工具两个服务，不修改模型容器的 Quadlet、镜像标签或运行状态。只修改 Agent / WebUI 时可用 `bash scripts/update.sh api`；不带参数才更新三项。首次安装仍执行 `scripts/start.sh`，修改 Pod / 网络等公共配置时执行完整安装流程。

CI 依据镜像输入文件选择重新构建的服务，并与上一次成功发布比较，覆盖被取消或失败的提交。未变服务复用已有 `latest`，只增加当前 SHA 标签；实际镜像内容不变，模型权重无需重新下载。`workflow_dispatch` 可手动执行完整构建。镜像复用遵循 [Docker 的单源 manifest 复制](https://docs.docker.com/reference/cli/docker/buildx/imagetools/create/)，使用 `--prefer-index=false` 保留原格式。只有实际构建的服务才生成新的离线归档。
