# 单一二进制：HTTP API + SQLite 长期记忆 + QQ 桥接，全部在一个 Rust 进程里。
# 模型调用全部走 DeepSeek 官方 API，容器内没有本地模型，也不需要模型缓存卷。
# 写完 .env 即可 docker run / compose up。
# rust:1 = 最新 stable；锁文件里的依赖会随更新抬高 rust-version 下限，别钉旧小版本
FROM rust:1-trixie AS builder

WORKDIR /src

# 先只拷贝依赖清单构建一次空壳，让依赖编译结果进缓存层。
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release --locked && \
    rm -rf src

COPY src ./src
COPY static ./static
RUN touch src/main.rs && cargo build --release --locked

FROM debian:trixie-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --create-home --uid 10001 appuser && \
    mkdir -p /data && \
    chown appuser:appuser /data

COPY --from=builder /src/target/release/mneme /usr/local/bin/mneme

USER appuser

# /data 存 SQLite 数据库——唯一需要持久化的东西。
VOLUME /data

EXPOSE 8000 9000
CMD ["mneme"]
