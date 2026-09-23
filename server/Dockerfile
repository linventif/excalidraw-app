# Multi-stage build producing a small, statically-linked (musl) image.
#
# Why musl via `rust:1-alpine` rather than cross-compiling from a glibc host:
# Alpine's own libc *is* musl, so building natively inside an Alpine
# container with the stock toolchain already produces a musl binary --
# there's no need to `rustup target add x86_64-unknown-linux-musl` or install
# a separate musl-gcc cross toolchain (which isn't available on the glibc dev
# host this was built on, and which is fiddlier to get working with sqlx's
# bundled SQLite C sources). This keeps the Dockerfile simple while still
# giving the "single small static binary" result the task asked for.

FROM rust:1-alpine AS builder

# musl-dev/build-base: C toolchain for sqlx-sqlite's bundled SQLite sources.
RUN apk add --no-cache musl-dev build-base

WORKDIR /app

# Cache dependency compilation separately from source changes.
COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

COPY src ./src
# Touch main.rs so cargo doesn't reuse the dummy build's stale binary.
RUN touch src/main.rs && cargo build --release

# ---- runtime image ----
FROM alpine:3.20

RUN apk add --no-cache ca-certificates \
    && adduser -D -u 10001 excalidraw \
    && mkdir -p /data \
    && chown -R excalidraw:excalidraw /data

COPY --from=builder /app/target/release/excalidraw-server /usr/local/bin/excalidraw-server

USER excalidraw
WORKDIR /data
VOLUME ["/data"]

ENV PORT=3002 \
    DATABASE_PATH=/data/excalidraw.sqlite \
    DATA_DIR=/data

EXPOSE 3002

# busybox wget ships with the alpine base image, no extra install needed.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget -q --spider "http://127.0.0.1:${PORT}/health" || exit 1

ENTRYPOINT ["excalidraw-server"]
