# 最小 Podman Quadlet 部署

这份 `qq-agent.container` 运行 GHCR 中的合并镜像——Neo4j、TEI Embedding、AI API 与 QQ BotGo 桥接全部在同一个容器内，不需要任何外部服务。数据保存在 `qq-agent-data`（Neo4j）、`qq-agent-models`（模型缓存）两个卷里。

## Rootless 安装

需要 Podman 5.x 与用户级 systemd：

```sh
mkdir -p ~/.config/containers/systemd
cp qq-agent.network qq-agent.container ~/.config/containers/systemd/
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

启用 Podman 自带的镜像自动更新定时器：

```sh
systemctl --user enable --now podman-auto-update.timer
```
