#!/bin/bash
# 单容器编排：Neo4j → TEI → AI API → QQ 桥接，按依赖顺序启动。
# 任一进程退出即整体退出，交给容器的 restart 策略拉起。
set -eu

: "${NEO4J_PASSWORD:?请先在 .env 设置 NEO4J_PASSWORD}"

# 兼容旧版多容器 .env：把 compose 服务名地址重映射为容器内回环地址。
if [ "${NEO4J_URI:-}" = "bolt://neo4j:7687" ]; then
  export NEO4J_URI=bolt://127.0.0.1:7687
fi
if [ "${EMBEDDING_BASE_URL:-}" = "http://embedding:80" ]; then
  export EMBEDDING_BASE_URL=http://127.0.0.1:8080
fi

pids=()
stopping=0
shutdown() {
  stopping=1
  kill -TERM "${pids[@]}" 2>/dev/null || true
}
trap shutdown TERM INT

wait_ready() { # wait_ready <名称> <重试次数> <间隔秒> <检查命令...>
  local name="$1" tries="$2" pause="$3"
  shift 3
  local i
  for ((i = 0; i < tries; i++)); do
    if [ "$stopping" -eq 1 ]; then
      exit 143
    fi
    local pid
    for pid in "${pids[@]}"; do
      if ! kill -0 "$pid" 2>/dev/null; then
        echo "等待 ${name} 期间有进程退出，放弃启动" >&2
        return 1
      fi
    done
    if "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep "$pause"
  done
  echo "等待 ${name} 就绪超时" >&2
  return 1
}

# ---- Neo4j ----
# 沿用官方 entrypoint（首次设密码、NEO4J_server_* 转配置、su-exec 降权）。
# 它会把所有 NEO4J_* 变量写进 neo4j.conf，而下面 env -u 摘掉的是 app 专用
# 变量，混进配置会触发 Neo4j 的严格配置校验、直接启动失败。
export NEO4J_AUTH="${NEO4J_USER:-neo4j}/${NEO4J_PASSWORD}"
export NEO4J_server_memory_heap_initial__size="${NEO4J_server_memory_heap_initial__size:-512m}"
export NEO4J_server_memory_heap_max__size="${NEO4J_server_memory_heap_max__size:-1G}"
export NEO4J_server_memory_pagecache_size="${NEO4J_server_memory_pagecache_size:-512m}"
export NEO4J_server_jvm_additional="${NEO4J_server_jvm_additional:---add-modules=jdk.incubator.vector}"
env -u NEO4J_URI -u NEO4J_USER -u NEO4J_PASSWORD -u NEO4J_DATABASE \
    -u NEO4J_IMAGE -u NEO4J_BROWSER_BIND_IP \
    /startup/docker-entrypoint.sh neo4j &
pids+=($!)

# ---- TEI 本地 embedding ----
# LD_PRELOAD/MKL 环境只对 TEI 进程生效，与官方 TEI CPU 镜像的 ENV 一致；
# 模型缓存放 /models（基底镜像的 /data 归 Neo4j）。首次启动会下载约 1.2GB 模型。
su-exec appuser:appuser env \
  LD_PRELOAD=/usr/local/libfakeintel.so \
  LD_LIBRARY_PATH=/usr/local/lib \
  MKL_ENABLE_INSTRUCTIONS=AVX512_E4 \
  RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-8}" \
  HUGGINGFACE_HUB_CACHE=/models \
  HF_TOKEN="${HF_TOKEN:-}" \
  text-embeddings-router \
    --model-id "${EMBEDDING_MODEL:-Qwen/Qwen3-Embedding-0.6B}" \
    --max-batch-tokens "${EMBEDDING_CONTEXT_SIZE:-32768}" \
    --max-client-batch-size "${EMBEDDING_MAX_CLIENT_BATCH_SIZE:-16}" \
    --hostname 127.0.0.1 --port 8080 --json-output &
pids+=($!)

# AI API 启动时要连 Neo4j 建索引，必须等 Bolt 就绪；embedding 按请求懒用，不阻塞。
wait_ready "Neo4j" 90 2 cypher-shell -a bolt://127.0.0.1:7687 \
  -u "${NEO4J_USER:-neo4j}" -p "${NEO4J_PASSWORD}" "RETURN 1"

# ---- AI API ----
cd /app
su-exec appuser:appuser /opt/venv/bin/uvicorn src.main:app \
  --host 0.0.0.0 --port 8000 --proxy-headers &
pids+=($!)

wait_ready "AI API" 60 1 curl -fsS http://127.0.0.1:8000/health/live

# ---- QQ 桥接 ----
su-exec appuser:appuser qqbot &
pids+=($!)

wait -n "${pids[@]}" && status=0 || status=$?
shutdown
wait || true
exit "$status"
