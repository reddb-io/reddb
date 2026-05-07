# null: DiskSpaceMonitor: edge-triggered fanotify replacing check_disk_headroom polling

## What to build

Substituir `runtime/resource_limits.rs::check_disk_headroom` (polling periódico) por edge-triggered watcher usando Linux `fanotify`. Kernel notifica imediatamente quando filesystem cruza threshold (90% por default). Handler dispara `OperatorEvent::DiskSpaceCritical` (já wired em #205).

Linux-first per memory rule. macOS / outros = stub que mantém polling antigo (compatibility shim, não otimizado).

Pattern: edge-triggered interrupt. Não polla — kernel acorda processo só quando estado muda. Zero CPU em condição saudável.

## Acceptance criteria

- [x] `DiskSpaceMonitor` deep module em `crates/reddb-server/src/runtime/disk_space_monitor.rs`
- [x] Linux: usa `fanotify` syscall (via `libc` ffi direta) com threshold configurable
- [x] Non-Linux: cfg-gated stub que continua poll mode (preserva compatibilidade dev em macOS)
- [ ] Threshold configurable via `runtime.disk_space.critical_pct` (default 90) — hardcoded 90 at spawn site; config knob is follow-up
- [x] Dispara `OperatorEvent::DiskSpaceCritical { path, available_bytes, threshold_bytes }` no kernel notify
- [x] Hysteresis: não re-dispara quando flutuação cruza threshold várias vezes em janela curta (debounce 30s)
- [ ] Integration test: ramfs ou loopback FS — unit tests cover clamp + debounce logic; full loopback integration test is follow-up
- [x] `check_disk_headroom` kept como fallback on cfg-gated path (defined, not actively called by write path)
- [x] Documentar Linux requirement em `docs/operations/monitoring.md`

## Delivered

- `crates/reddb-server/src/runtime/disk_space_monitor.rs` — DiskSpaceMonitor struct, fanotify path (Linux), poll fallback, debounce, 4 unit tests
- `crates/reddb-server/src/runtime.rs` — `pub mod disk_space_monitor` registered
- `crates/reddb-server/src/service_cli.rs` — `DiskSpaceMonitor::new(watch_dir, 90).spawn()` wired after lease_loop boot
- `docs/operations/monitoring.md` — new doc covering fanotify requirement, threshold, event shape

## Notes for follow-up

- Config knob `runtime.disk_space.critical_pct` not yet plumbed through `RedDBOptions`
- Full loopback FS integration test (ramfs fill → event in <1s) deferred
