---
status: open
tag: AFK
---

# [AFK] Tier-wiring meta-slice (unblocks #471/#472/#473/#475/#477/#478)

Cross-cutting groundwork: thread the existing `StorageLayout` + `LayoutOverrides` types through `RedDBOptions` / `RedDBRuntime` so the six tier-flag issues can flip their toggles based on tier defaults instead of staying force-off.

## Background

`crates/reddb-server/src/storage/layout.rs` already defines:
- `StorageLayout` (Minimal/Standard/Performance/Max)
- `LayoutOverrides` (per-feature dedicated-dir + LogRoutingOverrides toggles)
- `TieredLayoutPaths::new(data_path, layout, overrides)` and `ensure_dirs()`

But `RedDBOptions` does not carry the layout, and no runtime path constructs `TieredLayoutPaths`. So every iter-1 of #471/#472/#473/#475/#477/#478 left the feature toggle force-off and put a `## Blocker — runtime tier wiring is absent` note in the issue.

## What to build

1. **RedDBOptions surface**: add `layout: StorageLayout` (default `Standard`) and `layout_overrides: LayoutOverrides` (default `LayoutOverrides::default()`). Public setters `with_layout(layout)` and `with_layout_overrides(overrides)`.

2. **Runtime construction**: in `RedDB::open_with_options` (or whichever path opens persistent DBs), construct `TieredLayoutPaths::new(&data_path, options.layout, options.layout_overrides.clone())` once. Call `ensure_dirs()`. Store the resolved paths on the runtime (likely on `RedDBInner`).

3. **Tier defaults**: derive the existing tier-flag toggles from `options.layout` when the operator hasn't explicitly overridden them:
   - `Max` enables: `.meta.json` sidecar (gh-472), `seq-N journal` (gh-473), tied to existing toggle setters.
   - `Performance`/`Max` route audit/slow logs to `<support_dir>/logs/` (gh-471).
   - `Standard` provisions `-shm` (gh-475).
   - All tiers can opt into `fold_pager_meta` (gh-477) and `fold_dwb_into_wal` (gh-478) via existing toggles; tier picks the default.

4. **Status accessor**: expose `runtime.status_log_destinations() -> (LogDestination, LogDestination)` and a CLI helper for `reddb status` to print "audit: <dest>, slow: <dest>".

## Acceptance criteria

- [ ] `RedDBOptions::with_layout` / `with_layout_overrides` compile and round-trip through `RedDB::open_with_options`.
- [ ] One test (`tests/e2e_tier_wiring.rs`) opens persistent DBs at each of Minimal/Standard/Performance/Max and asserts:
  - the toggle returned by each subsystem's status accessor matches the tier default, AND
  - `TieredLayoutPaths::ensure_dirs()` was called (logs_dir / shm path / journal path exist as appropriate per tier).
- [ ] The 6 tier-flag toggles (`.meta.json`, `seq-N journal`, audit/slow log dest, `-shm` provisioning, `fold_pager_meta`, `fold_dwb_into_wal`) honor tier defaults but can be overridden via existing per-feature setters.
- [ ] Existing tests still pass (no regression on default Standard tier).
- [ ] `reddb status` includes log destination per the #471 acceptance bullet.

## Out of scope

- Changing storage format, WAL layout, RemoteBackend trait, S3Backend, archiver/recovery internals.
- Persisting the chosen tier to disk (config layer can do that later).

## Notes

- `CARGO_TARGET_DIR=.target-gh-tier-wiring`
- Be surgical. The 6 tier-flag setters already exist and default to off — this slice flips defaults based on `options.layout`, it does not reimplement the toggles.
- Read iter-1 progress notes in `issues/blocked/471-...md` and `issues/{472,473,477,478}-*.md` (on main) before adding new public surface. They describe the missing seam in detail.
- Commit on success with a multi-Closes message: `Refs #471, #472, #473, #475, #477, #478` — keeping all 6 open until each is verified independently to have its tier-default behavior pinned.
- If a per-issue verification falls out of scope, leave the issue open with a clear iter-3 note pointing at this slice.

## Iter 1 — 2026-05-16 (un-verified, un-committed; bash + git denied this session)

Landed (worktree only — `cargo`/`git` both denied):

- `RedDBOptions` gained `layout: StorageLayout` + `layout_overrides: LayoutOverrides` fields with `with_layout` / `with_layout_overrides` setters, threaded through Debug / Clone / Default. (`crates/reddb-server/src/api.rs`)
- New `RedDBOptions::resolve_tiered_layout()` returns the `(data_path, TieredLayoutPaths)` pair; returns `None` for ephemeral options without a `data_path`.
- New `RedDBOptions::apply_tier_defaults()` flips the six process-global tier-flag toggles using the per-tier table from `apply_tier_defaults`'s doc comment:
  | toggle | minimal | standard | performance | max |
  |---|:-:|:-:|:-:|:-:|
  | `.meta.json` sidecar | off | off | off | on |
  | seq-N catalog journal | off | off | off | on |
  | `-shm` provisioning | off | **on** | **on** | on |
  | `fold_pager_meta` | off | off | off | on |
  | `fold_dwb_into_wal` | off | off | off | on |
  | audit/slow log destination | stderr | stderr | file | file |
  Retention: 32 at Max, 4 (OPT_IN baseline) elsewhere. Per-feature env hatches (`REDDB_META_JSON_SIDECAR=…` and friends) are still honored because they are read inside each getter, not here.
- New `api::tier_wiring` submodule with a `Mutex<Option<TieredLayoutPaths>>` cache + `current_layout_paths()` / `current_log_destinations()` helpers. `apply_tier_defaults` stashes the resolved paths so `red status` can surface them.
- `RedDB::open_with_options` now calls `options.apply_tier_defaults()` first thing, then `ensure_dirs()` for the resolved `TieredLayoutPaths`. Failures are logged at `warn!` and do not abort the open. (`crates/reddb-server/src/storage/unified/devx/reddb/impl_core_a.rs`)
- `red status` (JSON + plain) now prints `audit_log:` and `slow_log:` lines sourced from `tier_wiring::current_log_destinations()`. (`src/bin/red.rs`)
- `tests/e2e_tier_wiring.rs` (new, 5 cases serialised by a `Mutex`): per-tier defaults for Minimal/Standard/Performance/Max + `LayoutOverrides::logs` precedence test.
- `crates/reddb-server/src/lib.rs` re-exports the new `tier_wiring` module via the umbrella.

### Acceptance status

- [x] `RedDBOptions::with_layout` / `with_layout_overrides` compile and round-trip through `RedDB::open_with_options` (worktree code; not compiled).
- [x] One test (`tests/e2e_tier_wiring.rs`) opens persistent DBs at each of Minimal/Standard/Performance/Max and asserts toggles + ensure_dirs effects.
- [x] All 6 tier-flag toggles honor tier defaults; per-feature setters (and env hatches) still override.
- [ ] Existing tests still pass (no regression on default Standard tier) — **not run**.
- [x] `reddb status` includes log destination per #471.

### Decisions

- **Process-global toggle flips, not threaded `StorageLayout`.** The six toggle setters already exist and are read process-wide. Threading a `&TieredLayoutPaths` through every save callsite would balloon the diff for no behavioral benefit — `apply_tier_defaults` is the seam each iter-1 of #471/#472/#473/#475/#477/#478 explicitly asked for.
- **`Mutex<Option<TieredLayoutPaths>>` for status, not `OnceLock`.** The same process can open multiple databases at different tiers in sequence (test runner!), so the slot must be writable, not write-once.
- **Layout overrides win.** `TieredLayoutPaths::new` already applies `LayoutOverrides` on top of the preset; we just pass them through.

### Blockers / next iteration

- `cargo build`, `cargo check`, `cargo test`, and `git` all required approval in this Ralph run. The five-case e2e test was authored but not compiled. Next iter must:
  1. `CARGO_TARGET_DIR=.target-gh-tier-wiring cargo build -p reddb-server`
  2. `CARGO_TARGET_DIR=.target-gh-tier-wiring cargo test --test e2e_tier_wiring`
  3. Full repo `cargo test` to confirm no regression on default Standard tier (the only behavior change at Standard is `-shm` provisioning ON — `e2e_shm_provisioning.rs` already isolates that toggle, so the risk is per-issue tests that opened a persistent DB and assumed `-shm` would not appear next to the data file).
  4. `git add -A && git commit -m "feat(api): thread StorageLayout through RedDBOptions::open_with_options (refs #471, #472, #473, #475, #477, #478)"`

### Files touched (uncommitted)

- `crates/reddb-server/src/api.rs` — new fields, setters, `resolve_tiered_layout`, `apply_tier_defaults`, `tier_wiring` submodule.
- `crates/reddb-server/src/storage/unified/devx/reddb/impl_core_a.rs` — call into `apply_tier_defaults` + `ensure_dirs` at top of `open_with_options`.
- `crates/reddb-server/src/lib.rs` — re-export `tier_wiring`.
- `src/bin/red.rs` — `red status` audit_log / slow_log lines (JSON + plain).
- `tests/e2e_tier_wiring.rs` — new five-case integration test.

### Risk notes

- Standard-tier `-shm` provisioning flips ON. The existing `e2e_shm_provisioning.rs` covers the file lifecycle; any test that asserts the *absence* of `<data>.shm` next to a persistent DB will start failing. Search for `\.shm` assertions before merging.
- `LogDestination::File(...)` for Performance/Max writes inside the `<data>.rdb.red/` support tree. `TieredLayoutPaths::ensure_dirs` creates `logs/` for us, so audit/slow log sinks have a valid parent — but the actual sink wiring (who opens the file handle) is still on the gh-471 backlog.
