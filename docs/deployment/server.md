# Server Mode

In server mode, RedDB runs as a standalone process exposing a routed front-door or explicit HTTP, gRPC, and wire listeners.

## Starting

```bash
# Recommended simplest default: routed front-door on 127.0.0.1:5050
red server --path ./data/reddb.rdb

# Explicit HTTP + gRPC
red server \
  --path ./data/reddb.rdb \
  --grpc-bind 0.0.0.0:50051 \
  --http-bind 0.0.0.0:8080

# HTTP only
red server --http --path ./data/reddb.rdb --bind 0.0.0.0:8080

# gRPC only
red server --grpc --path ./data/reddb.rdb --bind 0.0.0.0:50051

# wire only
red server --path ./data/reddb.rdb --wire-bind 0.0.0.0:5051
```

## Characteristics

| Property | Value |
|:---------|:------|
| Transport | Router, HTTP, gRPC, wire, or a combination depending on flags |
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
sudo red service install \
  --binary /usr/local/bin/red \
  --path /var/lib/reddb/data.rdb \
  --grpc-bind 0.0.0.0:50051 \
  --http-bind 0.0.0.0:8080
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

## Maintenance Scheduling

Use an OS-level scheduler to call maintenance on a fixed interval:

```bash
# every 5 minutes
*/5 * * * * /usr/local/bin/red tick --bind 127.0.0.1:8080 --operations maintenance,retention,checkpoint
```

For production services, prefer a dedicated service/timer so you can observe failures:

```ini
# /etc/systemd/system/reddb-tick.service
[Unit]
Description=RedDB periodic maintenance tick

[Service]
Type=oneshot
ExecStart=/usr/local/bin/red tick --bind 127.0.0.1:8080 --operations maintenance,retention,checkpoint
```

```ini
# /etc/systemd/system/reddb-tick.timer
[Unit]
Description=Run RedDB maintenance every 5 minutes

[Timer]
OnUnitActiveSec=5m
Persistent=true

[Install]
WantedBy=timers.target
```

For Kubernetes, run the same command as a `CronJob` that targets the service DNS name.
