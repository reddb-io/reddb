# syntax=docker/dockerfile:1

# ============================================================================
# RedDB - Multi-stage Docker build
# ============================================================================
# Stage 1: Build release binaries with protobuf support
# Stage 2: distroless glibc runtime with non-root user
# ============================================================================

FROM rust:1.96-slim-bookworm AS builder

ARG DEBIAN_FRONTEND=noninteractive
ARG REDDB_CARGO_FEATURES=""

RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first for layer caching
COPY Cargo.toml Cargo.lock build.rs ./
COPY crates/ crates/
COPY docs/spec/ docs/spec/

# Create dummy binaries so dependency compilation is cached before source copy
RUN mkdir -p src/bin \
    && echo 'fn main() {}' > src/bin/red.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --locked --bin red ${REDDB_CARGO_FEATURES:+--features ${REDDB_CARGO_FEATURES}} 2>/dev/null || true \
    && rm -rf src

# Copy full source and build for real
COPY src/ src/

RUN cargo build --release --locked --bin red ${REDDB_CARGO_FEATURES:+--features ${REDDB_CARGO_FEATURES}} \
    && mkdir -p /image/data /image/etc/reddb \
    && touch /image/data/.keep /image/etc/reddb/.keep

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

COPY --from=builder --chown=nonroot:nonroot /app/target/release/red /usr/local/bin/red
COPY --from=builder --chown=nonroot:nonroot /image/data /data
COPY --from=builder --chown=nonroot:nonroot /image/etc/reddb /etc/reddb

WORKDIR /data
VOLUME /data
VOLUME /etc/reddb

# Wire (5050), gRPC (50051) and HTTP (8080) ports
EXPOSE 5050 50051 8080

ENV REDDB_DATA_PATH=/data/data.rdb
ENV REDDB_BIND_ADDR=0.0.0.0:5050
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
# REDDB_CONFIG_FILE overrides the default path if you need a non-standard
# location. Missing config files are treated as no-op overlays.

# Topology is intentionally not baked into the image. Use the same image for
# serverless, primary-replica, and cluster-shaped deployments; select the mode at
# runtime with args plus REDDB_STORAGE_PRESET / REDDB_STORAGE_PROFILE.

# === Secrets via file mounts ====================================================
# DO NOT bake REDDB_CERTIFICATE / REDDB_VAULT_KEY / REDDB_PASSWORD into this image.
# Mount them at runtime via Docker/Swarm secrets, K8s Secret volumes, or any
# orchestrator-native secret store. The binary honours the *_FILE convention:
#
#   docker run -v ./cert.txt:/run/secrets/reddb_certificate:ro reddb \
#     # OR explicitly:
#     -e REDDB_CERTIFICATE_FILE=/run/secrets/reddb_certificate
#
# See examples/docker-compose.vault.yml for the canonical secure deploy.
# ===============================================================================

USER nonroot:nonroot

# PLAN.md (cloud-agnostic) Phase 1 — universal liveness probe.
# `/health/live` is the orchestrator-facing endpoint that every
# runtime (K8s, Docker, Fly, ECS, Nomad, systemd) understands. It
# returns 200 while the process is responsive, 503 only after
# Stopped. Cheap — no I/O.
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/red", "health", "--http", "--bind", "127.0.0.1:8080"]

ENTRYPOINT ["/usr/local/bin/red"]
CMD ["server", "--http", "--vault", "--path", "/data/data.rdb", "--http-bind", "0.0.0.0:8080", "--wire-bind", "0.0.0.0:5050", "--grpc-bind", "0.0.0.0:50051"]
