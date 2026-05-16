---
status: open
tag: AFK
gh: 478
---

# [AFK] gh-478: Fold DWB into WAL via full-page-image records (feature flag)

GitHub: reddb-io/reddb#478

## What to build

Feature flag `fold_dwb_into_wal`. When ON: FPI records in WAL before first modification of each page per checkpoint cycle; no `-dwb` sidecar. OFF: DWB sidecar preserved. Recovery applies FPI before redo. Crash injection mid-page-write demonstrates clean recovery. Benchmark shows acceptable overhead in typical OLTP. ADR-child documents extended WAL record format.

## Acceptance criteria

- [ ] Flag `fold_dwb_into_wal` controls behavior
- [ ] ON: FPI records before first modification per checkpoint; `-dwb` not created
- [ ] OFF: DWB sidecar preserved
- [ ] Recovery applies FPI before redo
- [ ] Crash injection during page write demonstrates clean recovery
- [ ] Benchmark shows acceptable OLTP overhead
- [ ] ADR-child documents extended WAL record format

## Notes
- `CARGO_TARGET_DIR=.target-gh478`
- Commit `Closes #478` or `Refs` if partial
- Land FPI record format + recovery half; tier auto-enable defer.

## Progress (uncommitted — bash sandbox blocked git + cargo)

Code changes staged on worktree but not yet committed because `git` and
`cargo` were denied at the harness level during this Ralph iteration.
Next iteration: re-run with bash permissions, run
`CARGO_TARGET_DIR=.target-gh478 cargo test -p reddb e2e_fold_dwb_into_wal`
+ unit tests under `storage::wal::record`, then commit.

Files touched:
- `crates/reddb-server/src/storage/wal/record.rs` — new
  `RecordType::FullPageImage = 8` + `WalRecord::FullPageImage`
  variant; encode/decode + roundtrip + corruption tests.
- `crates/reddb-server/src/storage/wal/reader.rs` —
  `WalReader::collect_full_page_images()` helper returning
  latest-wins map of `page_id → (lsn, data)` for recovery.
- `crates/reddb-server/src/physical.rs` —
  `set_fold_dwb_into_wal_enabled` / `fold_dwb_into_wal_enabled`
  process-global toggle + `REDDB_FOLD_DWB_INTO_WAL` env hatch
  (mirrors `fold_pager_meta`).
- `crates/reddb-server/src/storage/engine/pager/impl.rs` — when
  flag ON, skip opening `-dwb` and remove any pre-existing
  sidecar (read-only opens leave the sidecar alone so legacy
  databases stay openable).
- `crates/reddb-server/src/lib.rs` — re-export new toggle pair.
- `tests/e2e_fold_dwb_into_wal_policy.rs` — covers OFF default
  preserves `-dwb`, ON-flip suppresses + cleans sidecar, FPI
  roundtrip through WAL file with latest-wins resolution.
- `docs/adr/0018-tiered-storage-layout.md` — new "Fold DWB into
  WAL" section documenting record layout, recovery contract,
  and explicit deferral list.

Acceptance status:
- [x] Flag `fold_dwb_into_wal` controls behavior.
- [x] ON: `-dwb` not created/opened; pre-existing removed.
- [x] OFF: DWB sidecar preserved.
- [x] FPI record format + WAL reader recovery helper landed.
- [ ] Recovery wired to pager open (currently helper only —
      deferred together with checkpoint-cycle FPI emission).
- [ ] Crash injection benchmark.
- [ ] Benchmark gate (OLTP overhead).
- [x] ADR-child documents extended WAL record format.

Blocker for full closure (same as #471/#472/#473/#475/#477): tier
auto-enable + RuntimeOptions wiring + pager flush-path FPI
emission. Land alongside that follow-up.
