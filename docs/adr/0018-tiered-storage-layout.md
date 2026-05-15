# ADR 0018: Tiered storage layout presets

Status: Proposed (2026-05-14)

Related: [ADR 0003: On-disk format v1.0 stable contract](0003-disk-format-v1.md)

## Context

RedDB currently derives sidecar files directly at individual callsites. WAL,
logical WAL, checkpoints, index artifacts, cache artifacts, and future metrics
state need a common layout contract before startup wiring can safely split those
artifacts across directories.

This is a database storage boundary, so path derivation must be boring and
predictable:

- constructing a layout must not touch disk;
- every path must be derived only from the configured `data_path`, the selected
  preset, and explicit overrides;
- directory creation must be a separate opt-in step;
- the default must preserve the operational shape expected by existing users
  until a later integration slice switches callsites over deliberately.

ADR 0003 defines stability and migration expectations for persisted bytes. This
ADR complements it by defining where future tier-aware artifacts live; it does
not change any on-disk byte format.

## Decision

Introduce four storage layout presets:

| Preset | Contract |
| --- | --- |
| `minimal` | Keep required durability sidecars beside the data file. No optional dedicated directories are enabled. |
| `standard` | Default. Keep WAL sidecars beside the data file, and use a deterministic support directory for snapshots and indexes. |
| `performance` | Use dedicated support subdirectories for WAL, snapshots, indexes, cache, and blobs. Temporary and metrics directories stay disabled. |
| `max` | Enable every known dedicated support subdirectory: WAL, snapshots, indexes, cache, blobs, temp, and metrics. |

Add a pure storage layout module with these public types:

- `StorageLayout` enum, serde-friendly, defaulting to `standard`.
- `LayoutOverrides` struct, serde-friendly, where every field is `Option<bool>`.
- `LayoutToggles`, the deterministic expanded form after applying the preset and
  overrides.
- `TieredLayoutPaths`, the derived path bundle for one `data_path`.

Preset expansion happens first, then overrides are applied field by field. That
makes a config like `layout = "performance"` plus `dedicated_wal_dir = false`
deterministic and easy to explain.

Path derivation uses a sibling support directory named:

```text
<data-file-name>.red
```

For example, `data/main.rdb` maps to `data/main.rdb.red`. Adjacent sidecar file
names keep the existing extension style (`main.rdb-uwal`, `main.rdb-tmp`) so the
future integration slice can bridge from current paths without inventing another
filename vocabulary.

The module exposes `ensure_dirs()` as the only API that creates directories.
Constructors, accessors, preset expansion, and `dirs_to_create()` are pure.

## Consequences

Positive:

- Startup code can be migrated in a later slice without re-litigating path
  naming.
- Tests can cover layout behavior without filesystem I/O.
- Operator config can start with one preset and override only the few toggles
  needed for a deployment.

Negative:

- `standard` introduces a support directory contract before all callsites use it.
  Until integration lands, this module is a declared foundation rather than the
  live runtime layout.
- Future artifact categories must be added deliberately to both the preset table
  and the expanded toggle type.

## Non-goals

- This ADR does not move existing WAL, checkpoint, cache, or index callsites.
- This ADR does not change the stable byte formats governed by ADR 0003.
- This ADR does not define remote object key layout for S3/Turso/D1 backends.

## Follow-up

The next implementation slice should thread `StorageLayout` through server
configuration and migrate the actual storage callsites to consume
`TieredLayoutPaths`, preserving compatibility for existing adjacent sidecars.
