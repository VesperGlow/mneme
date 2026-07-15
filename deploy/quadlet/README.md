# 最小 Podman Quadlet 部署

这份 `qq-agent.container` 运行 GHCR 中的合并镜像——AI API（内嵌 SQLite 存储与进程内 ONNX embedding）与 QQ BotGo 桥接同在一个容器内，不需要任何外部服务。数据保存在 `qq-agent-data`（SQLite）、`qq-agent-models`（模型缓存）两个卷里。

## Rootless 安装

需要 Podman 5.x 与用户级 systemd：

```sh
mkdir -p ~/.config/containers/systemd
cp qq-agent.container ~/.config/containers/systemd/
cp qq-agent.env.example ~/.config/containers/systemd/qq-agent.env
chmod 600 ~/.config/containers/systemd/qq-agent.env
```

编辑 `qq-agent.env`。当前 GHCR 包若保持私有，还需先登录：

```sh
podman login ghcr.io
```

加载并启动：

```sh
systemctl --user daemon-reload
systemctl --user enable --now qq-agent.service
loginctl enable-linger "$USER"
```

检查：

```sh
systemctl --user status qq-agent.service
curl http://127.0.0.1:8000/health
curl http://127.0.0.1:9000/healthz
```

示例 `qq-agent.env` 默认使用 `QQ_EVENT_MODE=websocket`，不需要公网域名或反向代理；只有改成 `webhook` 时，才需要把 QQ 开放平台的 HTTPS 回调反向代理到 `127.0.0.1:9000/qqbot`。

再精简一点的话，env 只需 8 行就能跑：`APP_API_KEY`、`AI_BASE_URL`、`AI_API_KEY`、`MEMORY_MODEL`、`CHAT_MODEL`、`QQ_APP_ID`、`QQ_APP_SECRET`、`QQ_EVENT_MODE=websocket`——存储路径、embedding 模型、记忆等级梯度等全部有代码默认值。

启用 Podman 自带的镜像自动更新定时器：

```sh
systemctl --user enable --now podman-auto-update.timer
```
