---
status: blocked
tag: AFK
gh: 471
---

# [AFK] gh-471: Route audit/slow logs to .rdb.d/logs/ in performance/max tiers

GitHub: reddb-io/reddb#471

## What to build

Move `audit.log` and `red-slow.log` to `<dbname>.rdb.d/logs/` when tier is `performance` or `max`. Routing configurable via `LayoutOverrides` (stderr/file path/syslog). Standard/minimal default = stderr; no log files in user's directory.

## Acceptance criteria

- [x] performance/max -> `<dbname>.rdb.red/logs/audit.log`, `<dbname>.rdb.red/logs/slow.log` (layout API)
- [x] standard/minimal default stderr; explicit file path via override
- [x] `logs/` dir created idempotently via `ensure_dirs()`
- [x] `TieredLayoutPaths` unit tests cover all tiers + overrides
- [ ] **BLOCKED**: runtime/CLI actually consume the routing decision
- [ ] **BLOCKED**: `reddb status` shows current log destination per log
- [ ] **BLOCKED**: telemetry integration tests against on-disk paths

## Progress (iter 1)

Implemented the pure-layout surface so a tracer for routing exists end-to-end inside `crates/reddb-server/src/storage/layout.rs`:

- New `LogDestination` enum (`Stderr` | `File(PathBuf)` | `Syslog`) with `describe()` for `reddb status` plumbing and `file_path()` accessor.
- New `LogRoutingOverrides { audit_log, slow_log }` nested inside `LayoutOverrides`.
- `StorageLayout::default_audit_log_in(&support_dir)` / `default_slow_log_in(...)` — `Performance` / `Max` default to `<support_dir>/logs/{audit,slow}.log`; `Standard` / `Minimal` default to stderr.
- `TieredLayoutPaths` now exposes `logs_dir`, `audit_log_destination`, `slow_log_destination`.
- `ensure_dirs()` idempotently creates `logs_dir` and the parent of any custom file destination (e.g. operator-set `/var/log/reddb`).
- Tests in `crates/reddb-server/tests/storage_layout.rs` cover: Standard stderr default, Performance file routing, Max dir list (now includes `logs/`), override → custom file path, override → force stderr on Performance.
- Breaking change: `LayoutOverrides` is no longer `Copy` (added owned `PathBuf` inside `LogDestination`). `StorageLayout::expand` now takes `&LayoutOverrides`. Only test sites consume `LayoutOverrides`; both updated.

## Blocker — runtime tier wiring is absent

`StorageLayout` is a pure type today; **no runtime path constructs `TieredLayoutPaths`**. Grepped for `StorageLayout|TieredLayoutPaths|storage_layout|red.storage.layout` across `crates/reddb-server/src` — every match is in `storage/layout.rs`, `storage/mod.rs`, or the test file. In particular:

- `RuntimeOptions` does not carry a `layout: StorageLayout` field.
- `crates/reddb-server/src/runtime/impl_core.rs:2111-2160` builds the audit logger via `AuditLogger::for_data_path(&data_path)` and the slow-query logger via `SlowQueryOpts { log_dir: data_path.parent() }`. Neither consults a layout.
- `crates/reddb-server/src/cli/commands.rs` `status` subcommand only knows `--bind`; it has no handle to the layout to print destinations.

Plugging tier-wiring through end-to-end is invasive (touches `RuntimeOptions`, config parsing, every audit-log call site, plus `red` CLI). Per the issue note, deferring to a follow-up. Next iteration should:

1. Add `layout: StorageLayout` and `layout_overrides: LayoutOverrides` to `RuntimeOptions` (read from `red.storage.layout` / `red.storage.overrides`).
2. Construct `TieredLayoutPaths` once in `RedDB::open_with_options`, call `ensure_dirs()`, store on the runtime.
3. Replace `AuditLogger::for_data_path` with a constructor that takes the resolved `LogDestination` (stderr → existing stderr fallback path; file → existing append-mode opener; syslog → new sink, can stub-and-warn initially).
4. Pass `slow_log_destination` into `SlowQueryOpts`.
5. Add `status_log_destinations()` accessor on the runtime and wire `red status` to print them (JSON + text).
6. Audit/telemetry integration tests already in `tests/audit_rotation.rs` should keep passing — they pass explicit paths; the new constructor must accept a path-or-stderr.

## Files changed (iter 1)

- `crates/reddb-server/src/storage/layout.rs` — new enum + overrides + tier defaults + path fields + `ensure_dirs` extension
- `crates/reddb-server/src/storage/mod.rs` — re-export `LogDestination`, `LogRoutingOverrides`
- `crates/reddb-server/tests/storage_layout.rs` — `expand(&overrides)` updates + 4 new tests covering routing

## Notes
- `CARGO_TARGET_DIR=.target-gh471`
- Commit with `Refs #471` (not `Closes`, partial)
- Could not run `cargo check` locally — sandbox denied `cargo` execution. Code reviewed by re-reading; no `cargo`-verified pass.
