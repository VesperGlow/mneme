#!/usr/bin/env sh
set -eu

cd "$(dirname "$0")/.."
if ! command -v docker >/dev/null 2>&1; then
  echo "没有找到 Docker。请先安装 Docker Engine 与 Compose 插件。" >&2
  exit 1
fi
if [ ! -f .env ]; then
  cp .env.example .env
  echo "已创建 .env。请先填入 DEEPSEEK_API_KEY、APP_API_KEY 与 QQ_APP_ID/QQ_APP_SECRET。"
  exit 0
fi

exec docker compose -f compose.yaml up -d --build

