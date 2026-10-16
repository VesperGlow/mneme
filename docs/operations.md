# 运维

## 观察与故障恢复

`GET /health/ready` 的 503 表示模型或图谱不可用，进程不会因此退出。外部 LLM 不在 readiness 中执行付费探测。日志默认不记录原始消息、完整上游错误体或密钥；任务失败使用稳定错误码，可结合失败种类、次数和本地服务日志定位。

检查：

```bash
systemctl --user status mneme.target mneme-api mneme-models mneme-neo4j
journalctl --user -u mneme-api -u mneme-models -u mneme-neo4j --since '15 min ago'
podman stats --no-stream mneme-api mneme-models mneme-neo4j
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

- `app.env` 和 `neo4j.env` 权限为 0600，配置目录为 0700；不要把 `.env` 或备份纳入 Git。具备本机同用户 Podman 权限的主体能够读取容器环境，按操作系统用户边界管理访问。
- 修改 API Token 或外部 LLM Key 后，重新执行安装脚本并重启 API。修改 `.env` 中的 Neo4j 密码不会修改已有数据卷里的认证信息，需要通过 Neo4j 管理命令先修改数据库密码，再同步环境配置。
- 预构建镜像仅支持 `linux/amd64`，默认 `latest` 跟随 main。生产部署建议在拉取和安装前设置 `MNEME_IMAGE_TAG=sha-完整提交SHA`，固定 API 与模型镜像到同一提交；Neo4j tag、Rust `Cargo.lock`、Python constraints lock、模型权重及自定义代码 revision 均固定。升级依赖前运行全部常规与真实模型测试，备份 SQLite，再拉取或构建并重启。已发布迁移不可改写，新增编号迁移；数据库结构升级后回退二进制需要匹配备份。
- 初次成功下载之后模型缓存已经完整，可以给模型 Quadlet 添加 `Environment=HF_HUB_OFFLINE=1`；离线重启仍会从固定缓存加载模型。外部 LLM 仍需要网络。
- 若替换 Embedding 模型/维度，必须重新计算全部持久化向量并重建 Neo4j 索引，不能仅修改模型服务名称。v1 固定为 Qwen3 1024 维，不暴露不安全的热切换。

CPU 延迟依赖机器、文本长度和队列。默认 512 token、Embedding batch=2、Reranker batch=4 优先控制峰值；大量旧任务时先检查提供商配额和模型队列，再调整并发，避免在 4 核机器上让多个模型进程争抢 CPU。
