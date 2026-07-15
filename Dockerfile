# 单一镜像内包含全部四个组件：Neo4j（图 + 向量存储）、TEI（本地 embedding）、
# AI API（Python）、QQ 桥接（Go）。写完 .env 即可 docker run / compose up。
ARG NEO4J_IMAGE=neo4j:2026.05.0
ARG TEI_IMAGE=ghcr.io/huggingface/text-embeddings-inference:cpu-1.9

FROM golang:1.24-alpine AS gobuild

ARG GOPROXY=https://goproxy.cn,direct
ENV GOPROXY=${GOPROXY} CGO_ENABLED=0
WORKDIR /src

COPY qqbot/go.mod qqbot/go.sum ./
RUN go mod download
COPY qqbot/*.go ./
RUN go test ./... && go build -trimpath -ldflags="-s -w" -o /out/qqbot .

FROM ${TEI_IMAGE} AS tei

# 以官方 Neo4j 镜像为基底：保留它的 JVM、cypher-shell、su-exec 和
# docker-entrypoint.sh（负责首次设密码、环境变量转配置、降权运行）。
FROM ${NEO4J_IMAGE}

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    QQ_AI_URL=http://127.0.0.1:8000/v1/chat \
    HUGGINGFACE_HUB_CACHE=/models

# Python 运行时 + TEI 需要的 OpenMP/OpenSSL（与官方 TEI CPU 镜像的运行时依赖一致）
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      python3 python3-venv libomp-dev libssl-dev && \
    rm -rf /var/lib/apt/lists/*

# TEI：路由器二进制 + MKL 动态库 + Intel CPU 检测补丁，layout 与官方 CPU 镜像相同。
# TEI 基于 bookworm 构建、本镜像是 trixie（glibc 更新），二进制直接兼容；
# 下面的 --help 冒烟在构建期验证动态链接完整，缺库会立刻失败。
COPY --from=tei /usr/local/bin/text-embeddings-router /usr/local/bin/text-embeddings-router
COPY --from=tei /usr/local/lib/libmkl_*.so.2 /usr/local/lib/
COPY --from=tei /usr/local/libfakeintel.so /usr/local/libfakeintel.so
RUN command -v su-exec tini cypher-shell >/dev/null && \
    LD_LIBRARY_PATH=/usr/local/lib text-embeddings-router --help >/dev/null

WORKDIR /app

COPY app/requirements.txt .
RUN python3 -m venv /opt/venv && /opt/venv/bin/pip install --no-cache-dir -r requirements.txt

COPY app/src ./src
COPY app/static ./static
COPY --from=gobuild /out/qqbot /usr/local/bin/qqbot
COPY entrypoint.sh /usr/local/bin/entrypoint.sh

RUN chmod +x /usr/local/bin/entrypoint.sh && \
    useradd --create-home --uid 10001 appuser && \
    chown -R appuser:appuser /app && \
    mkdir -p /models && chown appuser:appuser /models

# /data /logs 由基底镜像声明；/models 缓存 embedding 模型
VOLUME /models

EXPOSE 8000 9000
ENTRYPOINT ["tini", "-g", "--", "/usr/local/bin/entrypoint.sh"]
