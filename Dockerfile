# syntax=docker/dockerfile:1

# ============================================================================
# RedDB - Multi-stage Docker build
# ============================================================================
# Stage 1: Build release binaries with protobuf support
# Stage 2: Minimal non-root runtime image
# ============================================================================

FROM rust:1.91-slim-bookworm AS builder

ARG DEBIAN_FRONTEND=noninteractive
ENV CARGO_PROFILE_RELEASE_STRIP=symbols

RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first for layer caching
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto/ proto/

# Create dummy binaries so dependency compilation is cached before source copy
RUN mkdir -p src/bin \
    && echo 'fn main() {}' > src/bin/red.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --locked --bin red 2>/dev/null || true \
    && rm -rf src

# Copy full source and build for real
COPY benches/ benches/
COPY src/ src/

RUN cargo build --release --locked --bin red

FROM debian:bookworm-slim

ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && groupadd --gid 10001 reddb \
    && useradd --uid 10001 --gid 10001 --no-create-home \
        --home-dir /nonexistent --shell /usr/sbin/nologin reddb \
    && install -d -o reddb -g reddb -m 0750 /data \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/red /usr/local/bin/red
COPY scripts/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod 0555 /usr/local/bin/red /usr/local/bin/docker-entrypoint.sh

WORKDIR /data
VOLUME /data

# gRPC (50051) and HTTP (8080) ports
EXPOSE 50051 8080

ENV REDDB_DATA_PATH=/data/data.rdb
ENV REDDB_BIND_ADDR=0.0.0.0:50051
ENV REDDB_GRPC_BIND_ADDR=0.0.0.0:50051
ENV REDDB_HTTP_BIND_ADDR=0.0.0.0:8080
ENV RUST_MIN_STACK=8388608

# Set these to auto-create the first admin user on startup:
# ENV REDDB_USERNAME=admin
# ENV REDDB_PASSWORD=changeme

USER reddb:reddb

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/red", "health", "--grpc", "--bind", "127.0.0.1:50051"]

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["server"]
