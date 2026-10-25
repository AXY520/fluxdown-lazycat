# syntax=docker/dockerfile:1.7
# ---------- 构建 ----------
FROM rust:1-bookworm AS builder
ARG TARGETARCH
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,id=ipadown-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=ipadown-target-${TARGETARCH},target=/app/target,sharing=locked \
    cargo build --locked --release -p ipadown-web \
    && cp /app/target/release/ipadown-web /tmp/ipadown-web

# ---------- 运行 ----------
FROM debian:bookworm-slim
ARG TARGETARCH
ARG IPATOOL_VERSION=2.7.0
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
# ipatool：SAP 签名登录/下载依赖（2026-08 起苹果强制 X-Apple-ActionSignature）
# 2.7.0 修复 redownload 请求被苹果 403 拒绝（空响应体）的问题
RUN arch="${TARGETARCH:-amd64}" \
    && case "$arch" in \
         amd64) expected="68372de9743d468296f2dba5b413a3e7df56ad0c85b8d0f4f0f6c9b8b53a54e8" ;; \
         arm64) expected="133c3fb754bafcd61698ba6047b48096c7033345529f57d7a83d281f8a403c23" ;; \
         *) echo "unsupported architecture: $arch" >&2; exit 1 ;; \
       esac \
    && curl -sfL --retry 5 --retry-all-errors --retry-delay 5 \
         "https://github.com/majd/ipatool/releases/download/v${IPATOOL_VERSION}/ipatool-${IPATOOL_VERSION}-linux-${arch}.tar.gz" -o /tmp/ipatool.tgz \
    && echo "$expected  /tmp/ipatool.tgz" | sha256sum -c - \
    && tar xzf /tmp/ipatool.tgz -C /tmp \
    && mv "/tmp/bin/ipatool-${IPATOOL_VERSION}-linux-${arch}" /usr/local/bin/ipatool \
    && chmod +x /usr/local/bin/ipatool \
    && rm /tmp/ipatool.tgz
COPY --from=builder /tmp/ipadown-web /usr/local/bin/ipadown-web

ENV IPADOWN_DATA_DIR=/data \
    PORT=8080
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD curl -sf http://localhost:${PORT}/healthz || exit 1

CMD ["ipadown-web"]
