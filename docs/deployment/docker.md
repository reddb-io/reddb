# Docker Deployment

RedDB ships as a single binary that runs inside a minimal Docker container.

## Build the Image

```bash
docker build -t reddb .
```

## Run HTTP Server

```bash
docker run --rm -it \
  -p 8080:8080 \
  -v $(pwd)/data:/data \
  --name reddb-http \
  reddb red server --http --path /data/reddb.rdb --bind 0.0.0.0:8080
```

## Run gRPC Server

```bash
docker run --rm -it \
  -p 50051:50051 \
  -v $(pwd)/data:/data \
  --name reddb-grpc \
  reddb red server --grpc --path /data/reddb.rdb --bind 0.0.0.0:50051
```

## Persist Data

Mount a volume to `/data` to persist the database across container restarts:

```bash
mkdir -p data

docker run -d \
  -p 8080:8080 \
  -v $(pwd)/data:/data \
  --restart unless-stopped \
  --name reddb \
  reddb red server --http --path /data/reddb.rdb --bind 0.0.0.0:8080
```

## Docker Compose

### Single Server

```yaml
version: '3.8'

services:
  reddb:
    build: .
    command: red server --http --path /data/reddb.rdb --bind 0.0.0.0:8080
    ports:
      - "8080:8080"
    volumes:
      - reddb-data:/data
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 10s
      timeout: 5s
      retries: 3

volumes:
  reddb-data:
```

### Primary + Replica

```yaml
version: '3.8'

services:
  primary:
    build: .
    command: >
      red server --grpc
        --path /data/primary.rdb
        --role primary
        --bind 0.0.0.0:50051
    ports:
      - "50051:50051"
    volumes:
      - primary-data:/data
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "red", "health", "--grpc", "--bind", "127.0.0.1:50051"]
      interval: 10s
      timeout: 5s
      retries: 3

  replica:
    build: .
    command: >
      red replica
        --primary-addr http://primary:50051
        --path /data/replica.rdb
        --http
        --bind 0.0.0.0:8080
    ports:
      - "8080:8080"
    volumes:
      - replica-data:/data
    depends_on:
      primary:
        condition: service_healthy
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 10s
      timeout: 5s
      retries: 3

volumes:
  primary-data:
  replica-data:
```

### With Auth Vault

```yaml
version: '3.8'

services:
  reddb:
    build: .
    command: >
      red server --http
        --path /data/reddb.rdb
        --bind 0.0.0.0:8080
        --vault
    ports:
      - "8080:8080"
    volumes:
      - reddb-data:/data
    restart: unless-stopped

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

RedDB currently uses CLI flags rather than environment variables. Pass flags via the Docker `command`:

```bash
docker run -d \
  reddb red server \
    --http \
    --path /data/reddb.rdb \
    --bind 0.0.0.0:8080 \
    --vault
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

## Resource Recommendations

| Workload | CPU | Memory | Disk |
|:---------|:----|:-------|:-----|
| Development | 1 core | 256 MB | 100 MB |
| Small production | 2 cores | 1 GB | 10 GB |
| Vector-heavy | 4+ cores | 4+ GB | 50+ GB |
| Graph analytics | 4+ cores | 8+ GB | 20+ GB |

> [!WARNING]
> Always mount a persistent volume for production workloads. Without `-v`, data is lost when the container stops.
