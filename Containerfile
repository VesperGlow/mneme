FROM docker.io/library/rust:1.90-slim-trixie AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY web ./web
RUN cargo build --release --locked

FROM docker.io/library/debian:13-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/target/release/mneme /usr/local/bin/mneme
COPY config/default.toml /app/config/default.toml
ENV MNEME_LISTEN=0.0.0.0:8080 MNEME_DATABASE=/data/mneme.sqlite3
EXPOSE 8080
ENTRYPOINT ["mneme"]
