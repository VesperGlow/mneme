#!/usr/bin/env bash
# Disposable real services. Does not need an external LLM API key.
set -euo pipefail
cd "$(dirname "$0")/.."
cleanup() {
    podman rm -f mneme-models-smoke mneme-neo4j-smoke >/dev/null 2>&1 || true
}
if podman container exists mneme-models-smoke || podman container exists mneme-neo4j-smoke; then
    echo '检测到同名 smoke 容器，请先处理已有测试容器。' >&2
    exit 1
fi
trap cleanup EXIT
podman run -d --name mneme-models-smoke -p 127.0.0.1:18090:8090 \
    -v mneme-model-cache:/models localhost/mneme-models:0.1.0 \
    uvicorn app:app --host 0.0.0.0 --port 8090 --workers 1 --no-access-log
podman run -d --name mneme-neo4j-smoke -p 127.0.0.1:17474:7474 \
    -v mneme-neo4j-smoke:/data \
    -e NEO4J_AUTH=neo4j/mneme-smoke-only-password \
    -e NEO4J_server_memory_heap_initial__size=256m \
    -e NEO4J_server_memory_heap_max__size=512m \
    -e NEO4J_server_memory_pagecache_size=256m \
    -e NEO4J_server_bolt_enabled=false \
    docker.io/library/neo4j:5.26.32-community-trixie
python3 - <<'PY'
import time, urllib.request, urllib.error
for url in ('http://127.0.0.1:18090/health', 'http://127.0.0.1:17474/'):
    deadline = time.monotonic() + 1200
    while True:
        try:
            with urllib.request.urlopen(url, timeout=3):
                break
        except (urllib.error.URLError, OSError):
            if time.monotonic() >= deadline:
                raise SystemExit(f'服务启动超时：{url}')
            time.sleep(2)
PY
cargo test --locked --test real_services -- --ignored --nocapture
