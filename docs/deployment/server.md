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

## Bind Failure Contract

Bind addresses passed explicitly with `--http-bind`, `--grpc-bind`, or
`--wire-bind` are required. If one is already in use or cannot be opened,
startup fails with an explicit bind error.

Default listeners are best effort when another requested transport can still
serve traffic. If an implicit/default listener cannot bind, RedDB logs a
non-fatal degradation and keeps the successfully bound requested listener
running. HTTP health and readiness responses include `transport_listeners`
with `active` and `failed` entries so operators can see which listener
degraded and why.

## Production Checklist

- [ ] Use `--path` for persistent storage
- [ ] Enable `--vault` for authentication
- [ ] Set appropriate `--bind` address
- [ ] Configure health checks at `/health`
- [ ] Set up monitoring at `/stats`
- [ ] Enable snapshots for backup
- [ ] Configure systemd or Docker for restart policy
- [ ] Tune the data filesystem (see [Filesystem Tuning](#filesystem-tuning))

## Filesystem Tuning

RedDB writes its data file in 16 KiB pages (the engine `PAGE_SIZE`). Matching the
underlying filesystem's block/record size to that page avoids read-modify-write
amplification and keeps `fsync` cheap. Apply these at deploy time, before the
data file is created, on the dataset or file that holds `--path`.

### ZFS

```bash
# On the dataset backing --path, before first write
zfs set recordsize=16K rpool/reddb
```

ZFS defaults to a 128 KiB recordsize, so every 16 KiB page write forces a
read-modify-write of a full 128 KiB record — roughly **8× write amplification**.
Setting `recordsize=16K` aligns the record to the RedDB page and eliminates it.

### btrfs

```bash
# Disable copy-on-write on the data file (or its parent dir before creation)
chattr +C /var/lib/reddb/data.rdb
```

btrfs copy-on-write relocates every overwritten block, fragmenting the data file
and doubling write traffic under RedDB's in-place page updates and WAL fsync.
`chattr +C` (nodatacow) keeps overwrites in place. Set it on an empty file or on
the parent directory before the data file is created — the flag only takes
effect for files created afterward.

### ext4 / XFS

```bash
# Default 4 KiB block size is fine; ensure the volume is mounted without atime churn
mount -o noatime /dev/sdX /var/lib/reddb
```

ext4 and XFS need no special tuning: their default 4 KiB block size divides the
16 KiB page evenly, so there is no record-level write amplification. Mount with
`noatime` to avoid metadata writes on every read.

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
