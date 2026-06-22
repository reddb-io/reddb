# Monitoring

## Disk Space (DiskSpaceMonitor)

RedDB spawns a background `DiskSpaceMonitor` at startup that watches the data
directory for low-disk conditions.

**Linux:** Uses `fanotify` (`FAN_CLASS_NOTIF | FAN_CLOSE_WRITE`) so the kernel
wakes the process only when a write closes — zero CPU when disk is healthy.
Requires `CAP_SYS_ADMIN` or a kernel with `fanotify` available to unprivileged
callers. In containers without that capability, the monitor falls back to a
30-second polling loop automatically (no configuration needed).

**Non-Linux (macOS, etc.):** Always uses the 30-second poll loop.

### Threshold

Default: emit `OperatorEvent::DiskSpaceCritical` when used% ≥ **90%**.  
Debounce: will not re-emit within 30 seconds of the previous emission.

The threshold is currently hardcoded at 90%. A `runtime.disk_space.critical_pct`
config knob is planned.

### Event

```
OperatorEvent::DiskSpaceCritical { path, available_bytes, threshold_bytes }
```

Inspect via the operator-event log or any registered `OperatorEventSink`.

## Normal KV Stats

Normal KV operation counters are process-local and reset on restart. They are
exposed in `/stats` under the `kv` object and in Prometheus format from
`/metrics`.

`/stats` keys:

- `kv.puts`
- `kv.gets`
- `kv.deletes`
- `kv.incrs`
- `kv.cas_success`
- `kv.cas_conflict`
- `kv.watch_streams_active`
- `kv.watch_events_emitted`
- `kv.watch_drops`

Prometheus metrics:

```prometheus
reddb_kv_ops_total{verb="put"}
reddb_kv_ops_total{verb="get"}
reddb_kv_ops_total{verb="delete"}
reddb_kv_ops_total{verb="incr"}
reddb_kv_cas_total{outcome="success"}
reddb_kv_cas_total{outcome="conflict"}
reddb_kv_watch_streams_active
reddb_kv_watch_events_emitted_total
reddb_kv_watch_drops_total
```

Scrape recipe:

```yaml
scrape_configs:
  - job_name: reddb
    metrics_path: /metrics
    static_configs:
      - targets: ['127.0.0.1:5000']
```

Useful alerts:

```prometheus
increase(reddb_kv_cas_total{outcome="conflict"}[5m]) > 100
increase(reddb_kv_watch_drops_total[5m]) > 0
```

`red doctor` also includes a `kv_stats` check that prints the current
operation counters and watch drop count from the same metrics surface.
