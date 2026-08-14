FROM golang:1.25-alpine AS build

ARG BUILD_COMMIT=unknown

WORKDIR /src

COPY go.mod go.sum ./
RUN go mod download

COPY cmd ./cmd
COPY internal ./internal
COPY prompts ./prompts

RUN CGO_ENABLED=0 GOOS=linux go build \
    -trimpath \
    -ldflags="-s -w -X main.buildCommit=${BUILD_COMMIT}" \
    -o /out/companion \
    ./cmd/companion

FROM alpine:3.22

RUN apk add --no-cache ca-certificates tzdata \
    && addgroup -S companion \
    && adduser -S -G companion companion \
    && mkdir -p /data \
    && chown companion:companion /data

COPY --from=build /out/companion /usr/local/bin/companion

USER companion
WORKDIR /data

ENV COMPANION_DB=/data/companion.db \
    COMPANION_ADDR=0.0.0.0:8787

VOLUME ["/data"]
EXPOSE 8787

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget -q -O /dev/null http://127.0.0.1:8787/health || exit 1

ENTRYPOINT ["companion"]
CMD ["serve"]
