# PRD: Tiered file layout

GitHub: https://github.com/reddb-io/reddb/issues/467
ADR: docs/adr/0018-tiered-storage-layout.md

## Problem

RedDB embedded currently presents one primary database file but leaks many
supporting files beside it: pager header and meta files, DWB, WAL files,
metadata snapshots, catalog journals, L2 caches, audit logs, slow logs, and
future operational artifacts. Users cannot easily tell which files are required
for backup, which files are optional, and which files are for diagnostics.

ADR 0003 defines the stable byte format inside persisted files. This PRD and
ADR 0018 define the files-in-directory contract: which files may exist, where
they live, and how RedDB moves from today's broad sidecar layout toward named
storage tiers.

## Product Goal

Expose a small set of storage layout presets that let users choose the
operational shape of an embedded database without tuning every sidecar
individually. The default should be easy to back up and explain, while advanced
tiers keep room for warm restart, forensics, and maximum observability.

## Tiers

| Tier | Intent | Visible shape |
| --- | --- | --- |
| `minimal` | Development and small embedded apps that want near-single-file ergonomics. | Data file plus WAL. |
| `standard` | Default production embedded mode with SQLite WAL-style ergonomics. | Data file, WAL, and shared memory file. |
| `performance` | Production mode that keeps hot restart artifacts but hides support files in a support directory. | Data file, WAL/SHM, and compact support directory. |
| `max` | Compatibility and forensics mode preserving today's broad support surface. | Current behavior plus explicit directory organization. |

The smaller tiers depend on invasive storage changes and must be enabled by
feature flags before becoming defaults. `max` is the compatibility anchor.

## Phased Delivery

### Phase A: Non-format layout cleanup

Phase A does not change page or WAL bytes. It establishes the layout module and
moves optional artifacts into explicit paths.

- Introduce the pure layout and tier config modules from ADR 0018.
- Route audit and slow logs into `.rdb.d/logs/` for `performance` and `max`.
- Unify result-cache and blob-cache L2 files into one `cache.rdb` support file.
- Stop writing `.meta.json` by default; replace it with an explicit catalog
  inspection command (`red inspect catalog --path <FILE> [--at <SEQ>]`).
- Make `seq-N` catalog journal snapshots opt-in outside `max`.

### Phase B: Standard tier

Phase B introduces the three-file default target after feature flags mature.

- Fold the pager header into page 0 of the datafile.
- Fold pager meta/free-list state into page 1 of the datafile.
- Fold DWB protection into WAL full-page-image records.
- Provision the `-shm` file for SQLite WAL-style shared state.

### Phase C: Minimal tier

Phase C enables the near-single-file development target.

- Embed the logical catalog as system pages inside the datafile.
- Recover catalog state from WAL replay instead of sidecar catalog snapshots.
- Keep the WAL as the only required durability sidecar.

## CLI and API Surface

Configuration exposes a layout preset plus fine-grained overrides:

- `storage.layout = "minimal" | "standard" | "performance" | "max"`
- `storage.overrides.*` for feature flags and artifact routing decisions
- embedded builder APIs equivalent to `with_layout(...)` and
  `with_layout_overrides(...)`

CLI expectations:

- `reddb status` reports the active tier and every path currently in use.
- `reddb inspect catalog` exports the human-readable catalog on demand and
  replaces automatic `.meta.json` writes.
- Layout flags must fail fast with clear errors when a tier requires feature
  flags that are not mature yet.

## Testing Strategy

The first implementation slices should test externally observable behavior and
pure configuration logic:

- Unit-test layout path derivation without filesystem I/O.
- Unit-test tier preset expansion and override precedence.
- Integration-test `max` as the compatibility tier.
- Integration-test startup failure messages for tiers whose required flags are
  not stable yet.
- Add phase-specific crash/recovery tests for pager page 0/1 folding,
  DWB-in-WAL, and embedded catalog recovery when those features land.

Passing generic engine tests is not enough for the invasive phases. Each storage
folding feature needs tests that reproduce realistic crash or recovery scenarios
for the durability invariant it changes.

## Non-goals

- Automatic migration from old layouts.
- Changing unrelated page, b-tree, MVCC, or SQL semantics.
- Removing forensic behavior from `max`.
- Replacing crash-injection tests with documentation-only claims.
- Adding new sidecars without an ADR 0018 update.

## Acceptance Criteria

- The repository has a durable PRD artifact for the tiered file layout plan.
- ADR 0018 remains the authoritative layout contract and is linked from this PRD.
- Future implementation issues can be validated against this phased plan.
- This PRD slice does not change runtime behavior.
