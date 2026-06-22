# Local Development with Docker

This guide gets a full RedDB dev environment running with one command.

Use it when you want:

- HTTP on `127.0.0.1:5000`
- RedWire on `127.0.0.1:5050`
- gRPC on `127.0.0.1:55055`
- one read replica on `127.0.0.1:5001`, `127.0.0.1:5051`, and `127.0.0.1:55056`
- persistent Docker volumes instead of local binaries

## 1. Start the stack

From the repository root:

```bash
docker compose -f examples/docker-compose.replica.yml up -d --build
```

If you want automatic smoke validation, use the dedicated test topology instead of the
manual `examples/` compose files:

```bash
make test-env PROFILE=replica
```

This uses
[examples/docker-compose.replica.yml](https://github.com/reddb-io/reddb/blob/main/examples/docker-compose.replica.yml).

The topology is:

- `primary`: RedWire `5050`, gRPC `55055`, HTTP `5000`
- `replica`: RedWire `5051`, gRPC `55056`, HTTP `5001`

## 2. Check health

```bash
curl -s http://127.0.0.1:5000/health
curl -s http://127.0.0.1:5001/health
```

For gRPC:

```bash
docker compose -f examples/docker-compose.replica.yml exec -T primary /usr/local/bin/red health --grpc --bind 127.0.0.1:55055
docker compose -f examples/docker-compose.replica.yml exec -T replica /usr/local/bin/red health --grpc --bind 127.0.0.1:55055
```

## 3. Write to the primary

```bash
curl -X POST http://127.0.0.1:5000/collections/hosts/rows \
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
curl -X POST http://127.0.0.1:5000/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts ORDER BY ip"}'
```

## 4. Read from the replica

Give replication a moment, then query the replica HTTP port:

```bash
curl -X POST http://127.0.0.1:5001/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts ORDER BY ip"}'
```

This is the simplest way to test read scaling locally:

- writes against `5000`
- reads against `5001`

## 5. Connect over gRPC

Primary:

```bash
red connect 127.0.0.1:55055
```

Replica:

```bash
red connect 127.0.0.1:55056
```

## 6. Watch the stack

```bash
docker compose -f examples/docker-compose.replica.yml ps
docker compose -f examples/docker-compose.replica.yml logs -f
```

To inspect just the replica:

```bash
docker compose -f examples/docker-compose.replica.yml logs -f replica
```

## 7. Reset everything

Stop containers only:

```bash
docker compose -f examples/docker-compose.replica.yml down
```

Stop and delete volumes:

```bash
docker compose -f examples/docker-compose.replica.yml down -v
```

That removes the local primary and replica data files under Docker volumes.

## 8. Use the larger topology

If you want one primary and two replicas:

```bash
docker compose -f examples/docker-compose.full.yml up -d --build
```

That stack exposes:

- primary RedWire `5050`, gRPC `55055`, HTTP `5000`
- replica-1 RedWire `5051`, gRPC `55056`, HTTP `5001`
- replica-2 RedWire `5052`, gRPC `55057`, HTTP `5002`

See also:

- [Docker Deployment](/deployment/docker.md)
- [Replication](/deployment/replication.md)
- [examples/README.md](https://github.com/reddb-io/reddb/blob/main/examples/README.md)
