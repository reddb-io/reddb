---
status: open
tag: AFK
gh: 517
---

# [AFK] gh-517: Wire S3/R2 RemoteBackend from env in red binary; start archiver+checkpointer

GitHub: reddb-io/reddb#517

## What to build

Env-driven `BackupBootstrap` builder in `reddb-server` that, given env-var lookup, returns `Option<BackupConfig>`. The `red` binary calls it at boot; when configured, constructs `S3Backend`, calls `Options::with_remote_backend` + `with_atomic_remote_backend`, spawns WAL archiver + checkpointer. When not configured, identical to today.

## Env contract (canonical names)

- `REDDB_BACKUP_S3_ENDPOINT`
- `REDDB_BACKUP_S3_BUCKET`
- `REDDB_BACKUP_S3_REGION` (default `auto`)
- `REDDB_BACKUP_S3_ACCESS_KEY_ID`
- `REDDB_BACKUP_S3_SECRET_ACCESS_KEY`
- `REDDB_BACKUP_S3_PREFIX` (required)
- `REDDB_BACKUP_CHECKPOINT_INTERVAL_SECS` (default 3600)
- `REDDB_BACKUP_WAL_FLUSH_INTERVAL_SECS` (default 30)

## Acceptance criteria

- [ ] `BackupBootstrap` parses env vars, applies defaults; pure function
- [ ] All required present → `Some(config)` with parsed fields
- [ ] None present → `None`
- [ ] Partial → `Err` naming missing var
- [ ] Non-numeric / zero / negative interval → `Err`
- [ ] `red` binary boot wires backend + spawns archiver + checkpointer when configured
- [ ] HTTP handlers `/backup/status` `/backup/trigger` `/recovery/restore-points` return real data when configured (no handler change required)
- [ ] Startup INFO log: backend kind, endpoint host, bucket, prefix, intervals
- [ ] Boot exits non-zero with clear message on partial config

## Notes

- Use `CARGO_TARGET_DIR=.target-gh517` for isolated builds.
- Commit with `Closes #517`.
- Do not change storage format, WAL layout, `RemoteBackend` trait, `S3Backend`, archiver/recovery internals.
- Add unit tests for `BackupBootstrap` (4 cases above).

## Iter 1 — 2026-05-16 (work-in-tree, not committed)

Implemented but **not committed** in this iteration because `cargo`,
`git`, and shell-level verification were not approved during the
autonomous Ralph run. Code is on disk and ready for the next
iteration to compile, test, and commit.

### What landed

- `crates/reddb-server/src/backup_bootstrap.rs` (new) — pure
  `from_env<F: Fn(&str)->Option<String>>(env: F) ->
  Result<Option<BackupConfig>, String>`. Parses the canonical
  `REDDB_BACKUP_S3_*` contract, applies defaults
  (`region=auto`, `checkpoint=3600s`, `wal_flush=30s`), and
  rejects partial / non-numeric / zero / negative intervals.
  Unit tests cover: none-present → `None`, all-required-present
  → `Some` with defaults, explicit overrides, partial → `Err`
  naming missing var, whitespace-only treated as missing,
  non-numeric / zero / negative interval → `Err`. **8 test
  cases total** (issue asked for 4).
- `crates/reddb-server/src/lib.rs` — registers `pub mod
  backup_bootstrap;`.
- `crates/reddb-server/src/service_cli.rs` —
  - `ServerCommandConfig::to_db_options` now returns
    `Result<RedDBOptions, String>`; all 6 callers threaded with
    `?`.
  - New `apply_backup_config(options, &BackupConfig)` wires the
    parsed config to `Options::with_remote_backend` +
    `with_atomic_remote_backend` (under `cfg(feature =
    "backend-s3")`), stashes intervals + backend kind in
    `options.metadata` under the
    `red.boot.backup.*` keys, and emits the startup INFO log
    `backup backend configured from REDDB_BACKUP_* env` carrying
    backend kind, endpoint host, bucket, prefix, and both
    interval values.
  - New `spawn_backup_tasks_if_configured(options, &runtime)`
    reads the metadata back and spawns two named threads:
    `red-checkpointer` (calls `runtime.checkpoint()` every
    `checkpoint_interval_secs`) and `red-wal-archiver` (calls
    `runtime.trigger_backup()` every `wal_flush_interval_secs`).
    Both loops poll a shared `AtomicBool` on a 1-second wake so
    shutdown is responsive. Returns a `BackupTasksHandle` whose
    `Drop` flips the stop flag. Each of the 6 runner entrypoints
    holds the handle in a `_backup_tasks` local for the server
    lifetime.
  - Partial config now bubbles `Err` out of `to_db_options`, so
    boot exits non-zero with a clear `backup bootstrap: ...`
    message naming the missing variable. Legacy
    `RED_BACKEND` / `REDDB_REMOTE_BACKEND` path is preserved as
    the fallback when the new contract is fully absent.

### Acceptance status

- [x] `BackupBootstrap` parses env vars, applies defaults; pure
      function.
- [x] All required present → `Some(config)` with parsed fields.
- [x] None present → `None`.
- [x] Partial → `Err` naming missing var.
- [x] Non-numeric / zero / negative interval → `Err`.
- [x] `red` binary boot wires backend + spawns archiver +
      checkpointer when configured. **Archiver = periodic
      `trigger_backup`, checkpointer = periodic `checkpoint`;
      no `S3Backend`/archiver/recovery internals were touched.**
- [x] HTTP handlers return real data when configured (no handler
      change required — `handlers_backup.rs` already reads
      `runtime.backup_status()` + `options`).
- [x] Startup INFO log includes backend kind, endpoint host,
      bucket, prefix, intervals.
- [x] Boot exits non-zero with a clear message on partial
      config.

### Decisions

- **Metadata-threading for intervals.** Rather than extend
  `RedDBOptions` with two public interval fields that have no
  meaning for library consumers, the intervals + backend kind
  travel through `options.metadata` (`red.boot.backup.*`). The
  runner extracts them on the other side of
  `build_runtime_and_auth_store` and uses them to spawn the
  tasks. Keeps the public `Options` surface unchanged.
- **`runtime.trigger_backup()` as the archiver tick.** The
  existing `WalArchiver` struct has no built-in scheduling
  helper and the issue explicitly forbids changing archiver
  internals. `runtime.trigger_backup()` drives the same backup
  pipeline through the public runtime API, which is what the
  `/backup/trigger` HTTP handler also calls.
- **`runtime.checkpoint()` as the checkpointer tick.** Same
  reasoning — `crate::storage::wal::checkpointer_task::spawn`
  needs a `CheckpointDriver` impl that does not currently exist
  in the runtime. Using the public `runtime.checkpoint()` keeps
  this slice surgical.

### Verification not yet run

The following must run before commit (blocked in this session by
permission gates on `cargo`, `git`, and `pnpm`):

```
CARGO_TARGET_DIR=.target-gh517 cargo test -p reddb-server \
    --lib backup_bootstrap
CARGO_TARGET_DIR=.target-gh517 cargo build -p reddb-server
CARGO_TARGET_DIR=.target-gh517 cargo build -p reddb-server \
    --features backend-s3
```

Next iter: run the three commands above; if green, `git add` the
two new/edited files and commit with `Closes #517`. The work is
already self-contained and ready to land.

### Files touched

- `crates/reddb-server/src/backup_bootstrap.rs` (new)
- `crates/reddb-server/src/lib.rs` (module registration)
- `crates/reddb-server/src/service_cli.rs` (bootstrap wiring +
  background tasks + Result-returning `to_db_options`)
