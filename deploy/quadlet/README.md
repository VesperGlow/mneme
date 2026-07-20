# 最小 Podman Quadlet 部署

这份 `mneme.container` 运行 GHCR 中的镜像——单个 Rust 二进制包含 AI API、进程内 ONNX embedding 与 rerank、SQLite 长期记忆与 QQ 桥接，不需要任何外部服务。数据保存在 `mneme-data`（SQLite）、`mneme-models`（模型缓存）两个卷里。

## Rootless 安装

需要 Podman 5.x 与用户级 systemd：

```sh
mkdir -p ~/.config/containers/systemd
cp mneme.container ~/.config/containers/systemd/
cp mneme.env.example ~/.config/containers/systemd/mneme.env
chmod 600 ~/.config/containers/systemd/mneme.env
```

编辑 `mneme.env`。当前 GHCR 包若保持私有，还需先登录：

```sh
podman login ghcr.io
```

加载并启动：

```sh
systemctl --user daemon-reload
systemctl --user enable --now mneme.service
loginctl enable-linger "$USER"
```

检查：

```sh
systemctl --user status mneme.service
curl http://127.0.0.1:8000/health
curl http://127.0.0.1:9000/healthz
```

示例 `mneme.env` 默认使用 `QQ_EVENT_MODE=websocket`，不需要公网域名或反向代理；只有改成 `webhook` 时，才需要把 QQ 开放平台的 HTTPS 回调反向代理到 `127.0.0.1:9000/qqbot`。

再精简一点的话，env 只需 8 行就能跑：`APP_API_KEY`、`AI_BASE_URL`、`AI_API_KEY`、`MEMORY_MODEL`、`CHAT_MODEL`、`QQ_APP_ID`、`QQ_APP_SECRET`、`QQ_EVENT_MODE=websocket`——存储路径、embedding 模型、记忆等级梯度等全部有代码默认值。

## 从旧的 qq-agent 部署切到 mneme

单元/容器名从 `qq-agent` 变成 `mneme`，卷名也从 `qq-agent-*` 改成 `mneme-*`。**卷改名后旧数据不会自动挂上，必须先迁移**，否则新容器挂到空卷、丢掉既有 `memory.db`。

先停旧服务再迁移卷（`memory.db` 用得着 `mneme-data`；`mneme-models` 只是模型缓存，不迁也会首启重新下载约 640MB）：

```sh
# 1. 停掉旧服务，释放卷
systemctl --user disable --now qq-agent.service

# 2. 把旧卷内容整体搬进同名的新卷（export/import 保留属主 uid 10001）
podman volume create mneme-data
podman volume export qq-agent-data | podman volume import mneme-data -
podman volume create mneme-models
podman volume export qq-agent-models | podman volume import mneme-models -

# 3. 移除旧单元，装新单元
rm ~/.config/containers/systemd/qq-agent.container ~/.config/containers/systemd/qq-agent.env
cp mneme.container ~/.config/containers/systemd/
cp mneme.env.example ~/.config/containers/systemd/mneme.env   # 或直接沿用旧 env 内容
chmod 600 ~/.config/containers/systemd/mneme.env
systemctl --user daemon-reload
systemctl --user enable --now mneme.service
```

确认新服务健康、`mneme memory list` 能看到旧记忆后，再删旧卷收尾：`podman volume rm qq-agent-data qq-agent-models`。镜像也换成了 `ghcr.io/vesperglow/mneme:latest`（新的 GHCR 包，首次可能私有，需 `podman login` 或在包设置里改公开）。

启用 Podman 自带的镜像自动更新定时器：

```sh
systemctl --user enable --now podman-auto-update.timer
```
