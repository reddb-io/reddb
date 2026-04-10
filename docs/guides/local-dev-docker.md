# Local Development with Docker

This guide gets a full RedDB dev environment running with one command.

Use it when you want:

- HTTP on `127.0.0.1:8080`
- gRPC on `127.0.0.1:50051`
- one read replica on `127.0.0.1:8081` and `127.0.0.1:50052`
- persistent Docker volumes instead of local binaries

## 1. Start the stack

From the repository root:

```bash
docker compose up -d --build
```

This uses
[docker-compose.yml](https://github.com/forattini-dev/reddb/blob/main/docker-compose.yml).

The topology is:

- `primary`: HTTP `8080`, gRPC `50051`
- `replica`: HTTP `8081`, gRPC `50052`

## 2. Check health

```bash
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8081/health
```

For gRPC:

```bash
docker compose exec -T primary /usr/local/bin/red health --grpc --bind 127.0.0.1:50051
docker compose exec -T replica /usr/local/bin/red health --grpc --bind 127.0.0.1:50051
```

## 3. Write to the primary

```bash
curl -X POST http://127.0.0.1:8080/collections/hosts/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "ip": "10.0.0.10",
      "role": "api",
      "critical": true
    }
  }'
```

Query it back:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts ORDER BY ip"}'
```

## 4. Read from the replica

Give replication a moment, then query the replica HTTP port:

```bash
curl -X POST http://127.0.0.1:8081/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts ORDER BY ip"}'
```

This is the simplest way to test read scaling locally:

- writes against `8080`
- reads against `8081`

## 5. Connect over gRPC

Primary:

```bash
red connect 127.0.0.1:50051
```

Replica:

```bash
red connect 127.0.0.1:50052
```

## 6. Watch the stack

```bash
docker compose ps
docker compose logs -f
```

To inspect just the replica:

```bash
docker compose logs -f replica
```

## 7. Reset everything

Stop containers only:

```bash
docker compose down
```

Stop and delete volumes:

```bash
docker compose down -v
```

That removes the local primary and replica data files under Docker volumes.

## 8. Use the larger topology

If you want one primary and two replicas:

```bash
docker compose -f docker-compose.full.yml up -d --build
```

That stack exposes:

- primary HTTP `8080`, gRPC `50051`
- replica-1 HTTP `8081`, gRPC `50052`
- replica-2 HTTP `8082`, gRPC `50053`

See also:

- [Docker Deployment](/deployment/docker.md)
- [Replication](/deployment/replication.md)
