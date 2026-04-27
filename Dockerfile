# syntax=docker/dockerfile:1

# ============================================================================
# RedDB - Multi-stage Docker build
# ============================================================================
# Stage 1: Build release binaries with protobuf support
# Stage 2: Debian-slim runtime with tini, non-root user, secret-file shim
# ============================================================================

FROM rust:1.91-slim-bookworm AS builder

ARG DEBIAN_FRONTEND=noninteractive
ARG REDDB_CARGO_FEATURES=""
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
    && cargo build --release --locked --bin red ${REDDB_CARGO_FEATURES:+--features ${REDDB_CARGO_FEATURES}} 2>/dev/null || true \
    && rm -rf src

# Copy full source and build for real
COPY benches/ benches/
COPY src/ src/

RUN cargo build --release --locked --bin red ${REDDB_CARGO_FEATURES:+--features ${REDDB_CARGO_FEATURES}}

FROM debian:bookworm-slim AS runtime

ARG DEBIAN_FRONTEND=noninteractive

# tini = init that reaps zombies and forwards SIGTERM to the engine
# (the engine's signal handler runs graceful shutdown — see docs/engine).
# curl is only here for HEALTHCHECK; it is not on the audited runtime path.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 reddb \
    && useradd --system --uid 10001 --gid 10001 \
        --home-dir /var/lib/reddb --no-create-home --shell /sbin/nologin reddb \
    && install -d -o 10001 -g 10001 -m 0750 /data \
    && install -o 10001 -g 10001 -m 0640 /dev/null /data/.keep \
    && install -d -o 10001 -g 10001 -m 0750 /run/secrets \
    && install -d -o 10001 -g 10001 -m 0755 /etc/reddb

COPY --from=builder --chown=10001:10001 /app/target/release/red /usr/local/bin/red
COPY --chown=10001:10001 docker/entrypoint.sh /entrypoint.sh
RUN chmod 0755 /entrypoint.sh

WORKDIR /data
VOLUME /data
VOLUME /etc/reddb

# gRPC (50051) and HTTP (8080) ports
EXPOSE 50051 8080

ENV REDDB_DATA_PATH=/data/data.rdb
ENV REDDB_BIND_ADDR=0.0.0.0:50051
ENV REDDB_GRPC_BIND_ADDR=0.0.0.0:50051
ENV REDDB_HTTP_BIND_ADDR=0.0.0.0:8080
ENV RUST_MIN_STACK=8388608

# Perf-parity config overlay — see docs/engine/perf-bench.md.
# RedDB self-heals the Tier A keys (durability.mode, concurrency.*,
# storage.wal.*, storage.bgwriter.*, storage.btree.lehman_yao) on
# first boot, so the image ships "opinionated by default" with no
# explicit ENV needed.
#
# To override a key on a running container:
#   docker run -e REDDB_DURABILITY_MODE=async -e REDDB_CONCURRENCY_LOCKING_ENABLED=true reddb
#
# To override via a mounted file instead:
#   docker run -v ./my-config.json:/etc/reddb/config.json reddb
# Format is JSON with dotted keys flattened: {"durability":{"mode":"async"}}
#
# REDDB_CONFIG_FILE overrides the default path if you need a
# non-standard location.
ENV REDDB_CONFIG_FILE=/etc/reddb/config.json

# === Secrets via file mounts ====================================================
# DO NOT bake REDDB_CERTIFICATE / REDDB_VAULT_KEY / REDDB_PASSWORD into this image.
# Mount them at runtime via Docker/Swarm secrets, K8s Secret volumes, or any
# orchestrator-native secret store. The entrypoint (/entrypoint.sh) and the
# binary both honour the *_FILE convention:
#
#   docker run -v ./cert.txt:/run/secrets/reddb_certificate:ro reddb \
#     # OR explicitly:
#     -e REDDB_CERTIFICATE_FILE=/run/secrets/reddb_certificate
#
# See examples/docker-compose.vault.yml for the canonical secure deploy.
# ===============================================================================

USER 10001:10001

# PLAN.md (cloud-agnostic) Phase 1 — universal liveness probe.
# `/health/live` is the orchestrator-facing endpoint that every
# runtime (K8s, Docker, Fly, ECS, Nomad, systemd) understands. It
# returns 200 while the process is responsive, 503 only after
# Stopped. Cheap — no I/O.
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -fsS --max-time 2 http://127.0.0.1:8080/health/live || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
CMD ["server", "--http", "--vault", "--path", "/data/data.rdb", "--http-bind", "0.0.0.0:8080", "--grpc-bind", "0.0.0.0:50051"]
