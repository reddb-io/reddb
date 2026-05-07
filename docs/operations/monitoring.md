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
