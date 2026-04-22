# Read Replica Tutorial

> [!NOTE]
> **Replica-side WAL consumption is live.** A replica started via
> `red replica --primary-addr ...` now polls `pull_wal_records`
> over gRPC on the primary, applies records locally, and persists
> `red.replication.last_applied_lsn` so restarts resume where they
> stopped. See `RedDBRuntime::run_replica_loop` in
> `src/runtime/impl_core.rs`. Automatic failover, synchronous
> replication, and multi-replica quorum are separate follow-ons
> and not yet wired.

This guide shows the smallest useful primary + replica setup for RedDB.

Goal:

- send writes to a primary
- send reads to a replica
- keep both HTTP and gRPC available on each node

## 1. Start the primary

```bash
mkdir -p ./data

red server \
  --path ./data/primary.rdb \
  --role primary \
  --grpc-bind 127.0.0.1:50051 \
  --http-bind 127.0.0.1:8080
```

The primary should always expose gRPC because replica streaming depends on it.

## 2. Start the replica

In another terminal:

```bash
red replica \
  --primary-addr http://127.0.0.1:50051 \
  --path ./data/replica.rdb \
  --grpc-bind 127.0.0.1:50052 \
  --http-bind 127.0.0.1:8081
```

Now you have:

- primary HTTP `8080`, gRPC `50051`
- replica HTTP `8081`, gRPC `50052`

## 3. Verify both nodes

```bash
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8081/health
```

```bash
red health --grpc --bind 127.0.0.1:50051
red health --grpc --bind 127.0.0.1:50052
```

## 4. Create data on the primary

```bash
curl -X POST http://127.0.0.1:8080/collections/orders/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "order_id": "ord-1001",
      "customer": "acme",
      "status": "open",
      "total": 1200
    }
  }'
```

## 5. Query the primary

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM orders"}'
```

## 6. Query the replica

Wait a moment for WAL shipping, then:

```bash
curl -X POST http://127.0.0.1:8081/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM orders"}'
```

At this point the replica is doing useful work: serving reads from its local copy.

## 7. Operational pattern

Recommended routing:

- writes: primary HTTP/gRPC
- health and metrics: all nodes
- read-only traffic: replicas
- CLI REPL: usually primary for admin work, replicas for query validation

## 8. Common mistakes

- Do not point writes at the replica.
- Do not start a replica without gRPC on the primary.
- Do not reuse the same `.rdb` file for both nodes.
- Do not test replication only on gRPC health; query the replica too.

## 9. Docker alternative

If you want the same topology in containers:

```bash
docker compose up -d --build
```

Then use:

- primary HTTP `127.0.0.1:8080`
- replica HTTP `127.0.0.1:8081`

See also:

- [Local Development with Docker](/guides/local-dev-docker.md)
- [Docker Deployment](/deployment/docker.md)
- [Replication](/deployment/replication.md)
