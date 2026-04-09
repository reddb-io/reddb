# Server Mode

In server mode, RedDB runs as a standalone process exposing HTTP and/or gRPC APIs.

## Starting

```bash
# HTTP server
red server --http --path ./data/reddb.rdb --bind 0.0.0.0:8080

# gRPC server
red server --grpc --path ./data/reddb.rdb --bind 0.0.0.0:50051

# Both (two processes)
red server --http --path ./data/reddb.rdb --bind 0.0.0.0:8080 &
red server --grpc --path ./data/reddb.rdb --bind 0.0.0.0:50051 &
```

## Characteristics

| Property | Value |
|:---------|:------|
| Transport | HTTP or gRPC |
| Latency | Microseconds (network) |
| Concurrency | Connection pool + async runtime |
| Persistence | File-backed with WAL |
| Auth | Optional vault-based auth |

## Production Checklist

- [ ] Use `--path` for persistent storage
- [ ] Enable `--vault` for authentication
- [ ] Set appropriate `--bind` address
- [ ] Configure health checks at `/health`
- [ ] Set up monitoring at `/stats`
- [ ] Enable snapshots for backup
- [ ] Configure systemd or Docker for restart policy

## Systemd

Install as a system service:

```bash
sudo ./scripts/install-systemd-service.sh \
  --binary /usr/local/bin/red \
  --http \
  --path /var/lib/reddb/data.rdb \
  --bind 0.0.0.0:8080
```

## Health Monitoring

```bash
# Liveness
curl http://localhost:8080/health

# Readiness
curl http://localhost:8080/ready

# Statistics
curl http://localhost:8080/stats
```
