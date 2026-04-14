# Docker Deployment

RedDB ships as a single binary that runs inside a minimal Docker container.

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
- `REDDB_GRPC_BIND_ADDR`
- `REDDB_HTTP_BIND_ADDR`
- `REDDB_BIND_ADDR` as a legacy fallback for gRPC

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

There is also a shared local harness for bringing these up and validating them:

```bash
make test-env PROFILE=replica
make test-env PROFILE=remote
make test-env PROFILE=serverless
```

That harness combines:

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
