# syntax=docker/dockerfile:1

# ============================================================================
# RedDB - Multi-stage Docker build
# ============================================================================
# Stage 1: Build release binaries with protobuf support
# Stage 2: Prepare owned runtime directories
# Stage 3: Distroless non-root runtime image
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

FROM debian:bookworm-slim AS runtime-prep

RUN install -d -o 10001 -g 10001 -m 0750 /data \
    && install -o 10001 -g 10001 -m 0640 /dev/null /data/.keep

FROM gcr.io/distroless/cc-debian12:latest

COPY --from=runtime-prep --chown=10001:10001 /data /data
COPY --from=builder --chown=10001:10001 /app/target/release/red /usr/local/bin/red

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

USER 10001:10001

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/red", "health", "--grpc", "--bind", "127.0.0.1:50051"]

ENTRYPOINT ["/usr/local/bin/red"]
CMD ["server"]
