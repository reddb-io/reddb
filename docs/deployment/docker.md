# Docker Deployment

RedDB ships as a single binary that runs inside a minimal Docker container.
The same image is used for serverless, primary-replica, and cluster-shaped
deployments. Runtime shape is selected by env vars, with command args reserved
for explicit overrides; do not use topology-specific images.

The container image defaults to the standard container listener contract:

- RedWire on `0.0.0.0:5050`
- gRPC on `0.0.0.0:55055`
- HTTP/Web/health on `0.0.0.0:5000`
- optional TLS/extra listener on `0.0.0.0:55555` when enabled
- data file at `/data/data.rdb`

The convention is intentional: product-facing/local development ports use the
`50xx` range, while infrastructure/control-plane listeners and local infra
emulator host ports use the `55xxx` range.

## Build the Image

```bash
docker build -t reddb .
```

## Run the Default Container

```bash
docker run --rm -it \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  -v $(pwd)/data:/data \
  --name reddb \
  reddb
```

The image `CMD` is intentionally just:

```dockerfile
CMD ["server"]
```

The image env supplies `REDDB_DATA_PATH=/data/data.rdb`,
`REDDB_WIRE_BIND_ADDR=0.0.0.0:5050`,
`REDDB_GRPC_BIND_ADDR=0.0.0.0:55055`,
`REDDB_HTTP_BIND_ADDR=0.0.0.0:5000`, and `REDDB_VAULT=false`.

## Container Topology Contract

Use one image and make the topology explicit:

| Topology | Command shape | Required storage env |
|---|---|---|
| `serverless` | `CMD ["server"]` | `REDDB_TOPOLOGY=serverless`, `REDDB_NODE_ROLE=serverless`, `REDDB_STORAGE_PRESET=serverless` |
| `primary-replica` primary | `CMD ["server"]` | `REDDB_TOPOLOGY=primary-replica`, `REDDB_NODE_ROLE=primary`, `REDDB_STORAGE_PRESET=primary-replica-production-ha` |
| `primary-replica` replica | `CMD ["server"]` | same storage env plus `REDDB_NODE_ROLE=replica` and `REDDB_PRIMARY_ADDR=http://primary:55055` |
| `cluster` | `CMD ["server"]` | `REDDB_TOPOLOGY=cluster`, `REDDB_NODE_ROLE=cluster-member`, `REDDB_STORAGE_PRESET=cluster`, cluster identity/discovery env |

`embedded` is a library/local mode and is not a separate production container
topology.

The common env contract is:

```bash
REDDB_TOPOLOGY=serverless|primary-replica|cluster
REDDB_NODE_ROLE=serverless|primary|replica|cluster-member
REDDB_STORAGE_PRESET=serverless|primary-replica-production-ha|cluster
REDDB_STORAGE_PROFILE=serverless|primary-replica|cluster
REDDB_STORAGE_PACKAGING=operational-directory
REDDB_REPLICA_COUNT=3
REDDB_PRIMARY_ADDR=http://primary:55055
REDDB_CONFIG_FILE=/etc/reddb/config.json
REDDB_VAULT=false|true
```

The binary consumes `REDDB_TOPOLOGY`, `REDDB_NODE_ROLE`, and
`REDDB_PRIMARY_ADDR` as the human topology layer when explicit args are absent.
They compile into the process role, replica upstream, and storage defaults.
`REDDB_CONFIG_FILE` is resolved by the same operational bootstrap contract; the
runtime still applies the file after storage opens so write-if-absent
`red.config` semantics are preserved:

- `serverless` runs the standalone process role with serverless storage.
- `primary-replica` uses `REDDB_NODE_ROLE=primary|replica` to select the
  process role.
- `cluster` runs the standalone process role today with cluster storage and
  cluster discovery env.

Explicit CLI args still win. `REDDB_STORAGE_*` overrides the topology-derived
storage default, and `REDDB_CLUSTER_*` remains the cluster identity/discovery
contract.

## Persist Data

Mount a volume to `/data` to persist the database across container restarts:

```bash
mkdir -p data

docker run -d \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  -v $(pwd)/data:/data \
  --restart unless-stopped \
  --name reddb \
  reddb
```

## Override Transport Binds

Use environment variables when you want to keep the default entrypoint:

```bash
docker run --rm -it \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  -e REDDB_DATA_PATH=/data/reddb.rdb \
  -e REDDB_WIRE_BIND_ADDR=0.0.0.0:5050 \
  -e REDDB_GRPC_BIND_ADDR=0.0.0.0:55055 \
  -e REDDB_HTTP_BIND_ADDR=0.0.0.0:5000 \
  -v $(pwd)/data:/data \
  reddb
```

Or override the command directly:

```bash
docker run --rm -it \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  -v $(pwd)/data:/data \
  reddb server \
    --path /data/reddb.rdb \
    --wire-bind 0.0.0.0:5050 \
    --grpc-bind 0.0.0.0:55055 \
    --http-bind 0.0.0.0:5000
```

## Docker Compose

### Single Server

```yaml
services:
  reddb:
    build: .
    ports:
      - "5050:5050"
      - "55055:55055"
      - "5000:5000"
    volumes:
      - reddb-data:/data
    restart: unless-stopped
    environment:
      REDDB_TOPOLOGY: standalone
      REDDB_NODE_ROLE: standalone
      REDDB_STORAGE_PRESET: embedded
      REDDB_STORAGE_PROFILE: embedded
      REDDB_STORAGE_PACKAGING: single-file
      REDDB_REPLICA_COUNT: "0"
      REDDB_VAULT: "false"
    healthcheck:
      test: ["CMD", "red", "health", "--http", "--bind", "127.0.0.1:5000"]
      interval: 10s
      timeout: 5s
      retries: 3

volumes:
  reddb-data:
```

### Primary + Replica

```yaml
services:
  primary:
    build: .
    ports:
      - "5050:5050"
      - "55055:55055"
      - "5000:5000"
    volumes:
      - primary-data:/data
    restart: unless-stopped
    environment:
      REDDB_TOPOLOGY: primary-replica
      REDDB_NODE_ROLE: primary
      REDDB_STORAGE_PRESET: primary-replica-production-ha
      REDDB_STORAGE_PROFILE: primary-replica
      REDDB_STORAGE_PACKAGING: operational-directory
      REDDB_REPLICA_COUNT: "1"
      REDDB_VAULT: "false"
    healthcheck:
      test: ["CMD", "red", "health", "--http", "--bind", "127.0.0.1:5000"]
      interval: 10s
      timeout: 5s
      retries: 3

  replica:
    build: .
    ports:
      - "5051:5050"
      - "55056:55055"
      - "5001:5000"
    volumes:
      - replica-data:/data
    depends_on:
      primary:
        condition: service_healthy
    restart: unless-stopped
    environment:
      REDDB_TOPOLOGY: primary-replica
      REDDB_NODE_ROLE: replica
      REDDB_STORAGE_PRESET: primary-replica-production-ha
      REDDB_STORAGE_PROFILE: primary-replica
      REDDB_STORAGE_PACKAGING: operational-directory
      REDDB_REPLICA_COUNT: "1"
      REDDB_PRIMARY_ADDR: http://primary:55055
      REDDB_VAULT: "false"
    healthcheck:
      test: ["CMD", "red", "health", "--http", "--bind", "127.0.0.1:5000"]
      interval: 10s
      timeout: 5s
      retries: 3

volumes:
  primary-data:
  replica-data:
```

For full files that can be run directly:

```bash
docker compose -f examples/docker-compose.serverless.yml up --build
docker compose -f examples/docker-compose.replica.yml up --build
docker compose -f examples/docker-compose.cluster.yml up --build
```

### With Auth Vault

```yaml
services:
  reddb:
    build: .
    ports:
      - "5050:5050"
      - "55055:55055"
      - "5000:5000"
    volumes:
      - reddb-data:/data
    restart: unless-stopped
    environment:
      REDDB_TOPOLOGY: standalone
      REDDB_NODE_ROLE: standalone
      REDDB_STORAGE_PRESET: embedded
      REDDB_STORAGE_PROFILE: embedded
      REDDB_STORAGE_PACKAGING: single-file
      REDDB_REPLICA_COUNT: "0"
      REDDB_VAULT: "true"
      REDDB_CERTIFICATE_FILE: /run/secrets/reddb_certificate

volumes:
  reddb-data:
```

Bootstrap the admin user once with `red bootstrap`, store the printed
certificate as a Docker/Kubernetes secret, and then start this compose service.
The full runnable file is `examples/docker-compose.vault.yml`.

## Environment Variables

The binary consumes these environment variables directly:

- `REDDB_DATA_PATH`
- `REDDB_TOPOLOGY`
- `REDDB_NODE_ROLE`
- `REDDB_PRIMARY_ADDR`
- `REDDB_WIRE_BIND_ADDR`
- `REDDB_GRPC_BIND_ADDR`
- `REDDB_HTTP_BIND_ADDR`
- `REDDB_VAULT`
- `REDDB_BIND_ADDR` as a legacy `--bind` fallback for the routed front door or
  old single-transport mode
- `REDDB_STORAGE_PRESET`
- `REDDB_STORAGE_PROFILE`
- `REDDB_STORAGE_PACKAGING`
- `REDDB_REPLICA_COUNT`
- `RED_BACKEND`, `RED_REMOTE_KEY`, and backend-specific `RED_S3_*`,
  `RED_FS_PATH`, or `RED_HTTP_BACKEND_*`
- `RED_AUTO_RESTORE`, `RED_BACKUP_ON_SHUTDOWN`, `RED_LEASE_REQUIRED`,
  `RED_LEASE_TTL_SECS`, and `RED_LEASE_PREFIX` for serverless writers
- `RED_PRIMARY_COMMIT_POLICY`, `RED_PRIMARY_COMMIT_ACK_N`, and
  `RED_PRIMARY_COMMIT_DEADLINE_MS` for primary-replica durability policy

Secrets should come from orchestrator-native secret stores. For Kubernetes and
Docker secrets, either inject them with `valueFrom`/secret env vars or use the
`*_FILE` convention where supported, for example `RED_S3_SECRET_KEY_FILE` or
`REDDB_CERTIFICATE_FILE`.

## Config File Precedence

Mount JSON at `/etc/reddb/config.json` or set `REDDB_CONFIG_FILE` to another
path. The file is parsed on boot and seeds missing keys into `red.config`.

Separate boot/topology config from runtime config:

- Boot config must stay in args/env because it is needed before the DB opens:
  role, primary address, storage preset/profile, remote backend, lease, data
  path, and secret material.
- Runtime config is stored in `red.config` after boot.

For runtime config, env overrides for matrix keys such as
`REDDB_DURABILITY_MODE` win for the current boot and are not persisted. The
mounted config file writes missing keys into `red.config` with write-if-absent
semantics. Existing rows from a prior boot, `SET CONFIG`, or boot defaults are
not overwritten.

Use `SET CONFIG` or an explicit migration when a stored value must change.

You can still pass flags via the Docker `command` for ad hoc overrides:

```bash
docker run -d \
  reddb server \
    --path /data/reddb.rdb \
    --vault \
    --wire-bind 0.0.0.0:5050 \
    --grpc-bind 0.0.0.0:55055 \
    --http-bind 0.0.0.0:5000
```

## Health Checks

The `/health` endpoint returns HTTP 200 when healthy and 503 when degraded:

```bash
curl -f http://localhost:5000/health
```

For gRPC health checks, use the `red health` command:

```bash
red health --grpc --bind localhost:55055
```

## Local Dev and CI

The repository ships several compose topologies under `examples/`:

- `examples/docker-compose.min.yml`: single local server
- `examples/docker-compose.replica.yml`: primary + one read replica
- `examples/docker-compose.full.yml`: primary + two read replicas
- `examples/docker-compose.remote.yml`: primary + replica + Floci for remote snapshot/WAL archive testing
- `examples/docker-compose.backup.yml`: single server + Floci for remote backup flows
- `examples/docker-compose.pitr.yml`: single primary + Floci for PITR and restore-point flows
- `examples/docker-compose.serverless.yml`: single remote-backed serverless-style node + Floci
- `examples/docker-compose.cluster.yml`: three symmetric cluster-shape members

`examples/` is for manual usage and documentation. The automated test harness uses a separate
test-only compose tree under `testdata/compose/`.

There is also a shared local harness for bringing up those test environments and validating them:

```bash
make test-env PROFILE=replica
make test-env PROFILE=remote
make test-env PROFILE=serverless
```

That test harness combines:

- shell checks for health/readiness/control-plane endpoints
- Rust integration tests from `tests/integration_external_env.rs`

To keep an environment up and iterate on the Rust tests only:

```bash
KEEP_UP=1 make test-env-shell PROFILE=replica
make test-env-rust PROFILE=replica
```

Typical local-dev loop:

```bash
docker compose -f examples/docker-compose.replica.yml up -d --build
docker compose -f examples/docker-compose.replica.yml logs -f
curl -s http://127.0.0.1:5000/health
curl -s http://127.0.0.1:5001/health
docker compose -f examples/docker-compose.replica.yml down -v
```

For a step-by-step walkthrough, see [Local Development with Docker](/guides/local-dev-docker.md).

For timeline/backup testing against an S3-compatible backend without cloud infrastructure:

```bash
docker compose -f examples/docker-compose.remote.yml up -d --build
docker compose -f examples/docker-compose.remote.yml logs -f
curl -s http://127.0.0.1:5000/replication/status
curl -s http://127.0.0.1:5001/replication/status
docker compose -f examples/docker-compose.remote.yml down -v
```

The dev topology provisions a local Floci S3 bucket and builds the RedDB image
with `backend-s3` enabled so snapshot/WAL archive flows can be exercised end-to-end.

## Resource Recommendations

| Workload | CPU | Memory | Disk |
|:---------|:----|:-------|:-----|
| Development | 1 core | 256 MB | 100 MB |
| Small production | 2 cores | 1 GB | 10 GB |
| Vector-heavy | 4+ cores | 4+ GB | 50+ GB |
| Graph analytics | 4+ cores | 8+ GB | 20+ GB |

> [!WARNING]
> Always mount a persistent volume for production workloads. Without `-v`, data is lost when the container stops.
