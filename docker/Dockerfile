# syntax=docker/dockerfile:1.7

# ort-sys 的官方预编译 ONNX Runtime 静态库依赖 glibc 2.38+ 与较新的 libstdc++ ABI。
# builder/runtime 统一使用 Debian 13，避免在 bookworm 上出现 __isoc23_strto*、
# __cxa_call_terminate 等未定义符号，并确保构建期与运行期 native ABI 一致。
ARG RUST_IMAGE=rust:1.96.0-trixie@sha256:58fe97504a0e4cbba5d85599619a589923d3e779472a6fb0840d58d1c4ba99d7
ARG RUNTIME_IMAGE=debian:trixie-slim@sha256:020c0d20b9880058cbe785a9db107156c3c75c2ac944a6aa7ab59f2add76a7bd

FROM ${RUST_IMAGE} AS builder

ARG QQ_MAID_BUILD_COMMIT=unknown
WORKDIR /build

COPY . .

# target/ 使用 BuildKit cache；最终二进制先复制到非缓存目录，供运行阶段取用。
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    QQ_MAID_BUILD_COMMIT="${QQ_MAID_BUILD_COMMIT}" \
    cargo build --workspace --release --all-features --locked \
    && cp target/release/qq-maid-bot /tmp/qq-maid-bot

FROM ${RUNTIME_IMAGE} AS runtime

ARG QQ_MAID_BUILD_COMMIT=unknown
ARG QQ_MAID_BUILD_VERSION=dev
ARG QQ_MAID_BUILD_DATE=unknown

LABEL org.opencontainers.image.title="qq-maid-bot" \
      org.opencontainers.image.description="Self-hosted multi-platform AI agent bot" \
      org.opencontainers.image.source="https://github.com/kuliantnt/qq-maid-bot" \
      org.opencontainers.image.revision="${QQ_MAID_BUILD_COMMIT}" \
      org.opencontainers.image.version="${QQ_MAID_BUILD_VERSION}" \
      org.opencontainers.image.created="${QQ_MAID_BUILD_DATE}" \
      org.opencontainers.image.licenses="MIT"

# curl 只用于容器内 /healthz；CA、时区和 C++ 运行库是程序实际运行依赖。
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        libgcc-s1 \
        libstdc++6 \
        tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 qqmaid \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /app/runtime --shell /usr/sbin/nologin qqmaid \
    && install -d -o 10001 -g 10001 \
        /app/runtime/config \
        /app/runtime/data/storage \
        /app/runtime/media/inbound

COPY --from=builder --chown=10001:10001 /tmp/qq-maid-bot /usr/local/bin/qq-maid-bot

WORKDIR /app/runtime
USER 10001:10001

ENV TZ=Asia/Shanghai \
    APP_DB_FILE=data/storage/app.db \
    RUST_LOG=info,qq_maid_gateway_rs=debug

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=4 \
    CMD curl --fail --silent --show-error --max-time 3 "http://127.0.0.1:${LLM_SERVER_PORT:-8787}/healthz" >/dev/null

STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/qq-maid-bot"]
