# Docker Deployment

RedDB ships as a single binary that runs inside a minimal Docker container.
The same image is used for serverless, primary-replica, and cluster-shaped
deployments. Runtime shape is selected by command args and env vars, not by
using different images.

The container image now defaults to a dual-stack server:

- gRPC on `0.0.0.0:50051`
- HTTP on `0.0.0.0:8080`
- data file at `/data/data.rdb`

## Build the Image

```bash
docker build -t reddb .
```

## Run the Default Container

```bash
docker run --rm -it \
  -p 50051:50051 \
  -p 8080:8080 \
  -v $(pwd)/data:/data \
  --name reddb \
  reddb
```

That is equivalent to:

```bash
red server \
  --path /data/data.rdb \
  --grpc-bind 0.0.0.0:50051 \
  --http-bind 0.0.0.0:8080
```

## Container Topology Contract

Use one image and make the topology explicit:

| Topology | Command shape | Required storage env |
|---|---|---|
| `serverless` | `red server --path /data/data.rdb ...` | `REDDB_STORAGE_PRESET=serverless` |
| `primary-replica` primary | `red server --role primary --path /data/data.rdb ...` | `REDDB_STORAGE_PRESET=primary-replica-production-ha` |
| `primary-replica` replica | `red replica --primary-addr http://primary:50051 --path /data/data.rdb ...` | same storage preset plus `REDDB_PRIMARY_ADDR` |
| `cluster` | `red server --role standalone --path /data/data.rdb ...` today, with cluster identity/discovery env | `REDDB_STORAGE_PRESET=cluster` |

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
REDDB_CONFIG_FILE=/etc/reddb/config.json
```

The binary consumes `REDDB_TOPOLOGY` and `REDDB_NODE_ROLE` as the human
topology layer when explicit args are absent. They compile into the process role
and storage defaults. `REDDB_CONFIG_FILE` is resolved by the same operational
bootstrap contract; the runtime still applies the file after storage opens so
write-if-absent `red.config` semantics are preserved:

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
  -p 50051:50051 \
  -p 8080:8080 \
  -v $(pwd)/data:/data \
  --restart unless-stopped \
  --name reddb \
  reddb
```

## Override Transport Binds

Use environment variables when you want to keep the default entrypoint:

```bash
docker run --rm -it \
  -p 50051:50051 \
  -p 8080:8080 \
  -e REDDB_DATA_PATH=/data/reddb.rdb \
  -e REDDB_GRPC_BIND_ADDR=0.0.0.0:50051 \
  -e REDDB_HTTP_BIND_ADDR=0.0.0.0:8080 \
  -v $(pwd)/data:/data \
  reddb
```

Or override the command directly:

```bash
docker run --rm -it \
  -p 8080:8080 \
  -v $(pwd)/data:/data \
  reddb server --http --path /data/reddb.rdb --bind 0.0.0.0:8080
```

## Docker Compose

### Single Server

```yaml
services:
  reddb:
    build: .
    ports:
      - "50051:50051"
      - "8080:8080"
    volumes:
      - reddb-data:/data
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "red", "health", "--http", "--bind", "127.0.0.1:8080"]
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
      - "50051:50051"
      - "8080:8080"
    volumes:
      - primary-data:/data
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "red", "health", "--http", "--bind", "127.0.0.1:8080"]
      interval: 10s
      timeout: 5s
      retries: 3
    command:
      - "server"
      - "--path"
      - "/data/data.rdb"
      - "--role"
      - "primary"
      - "--grpc-bind"
      - "0.0.0.0:50051"
      - "--http-bind"
      - "0.0.0.0:8080"

  replica:
    build: .
    ports:
      - "50052:50051"
      - "8081:8080"
    volumes:
      - replica-data:/data
    depends_on:
      primary:
        condition: service_healthy
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "red", "health", "--http", "--bind", "127.0.0.1:8080"]
      interval: 10s
      timeout: 5s
      retries: 3
    command:
      - "replica"
      - "--primary-addr"
      - "http://primary:50051"
      - "--path"
      - "/data/data.rdb"
      - "--grpc-bind"
      - "0.0.0.0:50051"
      - "--http-bind"
      - "0.0.0.0:8080"

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
      - "50051:50051"
      - "8080:8080"
    volumes:
      - reddb-data:/data
    restart: unless-stopped
    command:
      - "server"
      - "--path"
      - "/data/reddb.rdb"
      - "--vault"
      - "--grpc-bind"
      - "0.0.0.0:50051"
      - "--http-bind"
      - "0.0.0.0:8080"

volumes:
  reddb-data:
```

After starting, bootstrap the admin user:

```bash
curl -X POST http://localhost:8080/auth/bootstrap \
  -H 'content-type: application/json' \
  -d '{"username": "admin", "password": "changeme"}'
```

## Environment Variables

The container entrypoint supports these environment variables:

- `REDDB_DATA_PATH`
- `REDDB_TOPOLOGY`
- `REDDB_NODE_ROLE`
- `REDDB_GRPC_BIND_ADDR`
- `REDDB_HTTP_BIND_ADDR`
- `REDDB_BIND_ADDR` as a legacy fallback for gRPC
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

You can also pass flags via the Docker `command`:

```bash
docker run -d \
  reddb server \
    --path /data/reddb.rdb \
    --vault \
    --grpc-bind 0.0.0.0:50051 \
    --http-bind 0.0.0.0:8080
```

## Health Checks

The `/health` endpoint returns HTTP 200 when healthy and 503 when degraded:

```bash
curl -f http://localhost:8080/health
```

For gRPC health checks, use the `red health` command:

```bash
red health --grpc --bind localhost:50051
```

## Local Dev and CI

The repository ships several compose topologies under `examples/`:

- `examples/docker-compose.min.yml`: single local server
- `examples/docker-compose.replica.yml`: primary + one read replica
- `examples/docker-compose.full.yml`: primary + two read replicas
- `examples/docker-compose.remote.yml`: primary + replica + MinIO for remote snapshot/WAL archive testing
- `examples/docker-compose.backup.yml`: single server + MinIO for remote backup flows
- `examples/docker-compose.pitr.yml`: single primary + MinIO for PITR and restore-point flows
- `examples/docker-compose.serverless.yml`: single remote-backed serverless-style node + MinIO

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
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8081/health
docker compose -f examples/docker-compose.replica.yml down -v
```

For a step-by-step walkthrough, see [Local Development with Docker](/guides/local-dev-docker.md).

For timeline/backup testing against an S3-compatible backend without cloud infrastructure:

```bash
docker compose -f examples/docker-compose.remote.yml up -d --build
docker compose -f examples/docker-compose.remote.yml logs -f
curl -s http://127.0.0.1:8080/replication/status
curl -s http://127.0.0.1:8081/replication/status
docker compose -f examples/docker-compose.remote.yml down -v
```

The dev topology provisions a local MinIO bucket and builds the RedDB image
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
