# null: InotifyConfigReload: hot-reload server config without restart

## What to build

Adicionar inotify watcher no arquivo de config (`config.toml`). On change event (atomic rename-swap pattern), re-parse + apply hot-reload sem restart.

Hoje config exige restart pra mudanças. Operador rodando produção quer ajustar log level, slow_query threshold, ou disk_space_pct sem janela de manutenção.

Subset de campos hot-reloadable. Outros (binding ports, storage paths) requerem restart e geram OperatorEvent::ConfigChangeRequiresRestart.

Linux-first via inotify. macOS = stub que ignora ou usa kqueue (out of scope MVP).

## Acceptance criteria

- [x] `ConfigWatcher` deep module em `crates/reddb-server/src/runtime/config_watcher.rs`
- [x] inotify watch no path do config file passed at startup (`REDDB_CONFIG_FILE` or `/etc/reddb/config.json`)
- [x] Atomic rename-swap detection (vim default save pattern) handled corretamente via `IN_MOVED_TO`
- [x] Hot-reloadable fields enumerated (whitelist explícita): `red.logging.*`, `slow_query.*`, `disk_space.critical_pct`
- [x] Non-hot-reloadable fields detectados → emit `OperatorEvent::ConfigChangeRequiresRestart { fields_changed }`, ignora apply
- [x] OperatorEvent::ConfigChanged disparado on successful hot-reload com diff (old/new values)
- [x] Hot-reload atomic: ou tudo aplica, ou rollback — parse-then-apply; se parse falha, nada aplica
- [ ] Integration test: write new config file → assert change applied dentro de 1s sem restart — deferred (requires in-process store access from test)
- [ ] Document hot-reloadable subset em `docs/operations/config.md` — deferred

## Delivered

- `crates/reddb-server/src/runtime/config_watcher.rs` — ConfigWatcher struct, inotify path (Linux, IN_CLOSE_WRITE|IN_MOVED_TO), poll fallback (5s), apply_hot_reload with whitelist, 5 unit tests
- `crates/reddb-server/src/runtime.rs` — `pub mod config_watcher` registered
- `crates/reddb-server/src/service_cli.rs` — `ConfigWatcher::new(config_path, store).spawn()` wired after disk_space_monitor
- `crates/reddb-server/src/telemetry/operator_event.rs` — new `ConfigChangeRequiresRestart { fields_changed }` variant + decompose arm

## Notes for follow-up

- Integration test deferred: requires spawning ConfigWatcher with access to an in-memory store and triggering inotify event
- `docs/operations/config.md` documenting hot-reloadable fields deferred
- telemetry routes hot-reload (#I2) can be added to whitelist when that feature ships
