# ADR 0018: Tiered storage layout presets

Status: Proposed (2026-05-14)

Related: [ADR 0003: On-disk format v1.0 stable contract](0003-disk-format-v1.md)

Operator map: [Storage Profiles](../../docs/deployment/storage-profiles.md)
links these layout presets to `single-file`, `operational-directory`,
serverless segment packs, and cluster `range-directory` packaging.

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

## `<data>.shm` shared-memory substrate (gh-475)

The `standard` tier and above expose a `<data>-shm` sibling file as the lock
substrate for multi-reader embedded use, comparable in role to SQLite's
WAL-mode `-shm`. The on-disk shape is intentionally minimal so the future
mmap wiring slice has nothing to redesign.

### Binary layout (v1, little-endian, 64-byte fixed header)

| offset | size | field             | notes                                             |
| ------ | ---- | ----------------- | ------------------------------------------------- |
|      0 |    8 | magic             | ASCII `"RDBSHM01"`                                |
|      8 |    4 | version           | `u32 = 1`                                         |
|     12 |    4 | owner_pid         | host pid of the writer holding the lease          |
|     16 |    8 | generation        | bumped on every takeover or heal                  |
|     24 |    8 | reader_count      | attached embedded reader handles                  |
|     32 |    8 | last_heartbeat_ms | owner heartbeat in unix-ms                        |
|     40 |   16 | reserved          | zeroed; room for v2 fields                        |
|     56 |    8 | checksum          | FNV-1a fold over bytes `[0..56)`                  |

The full file is sized to one OS page (`4096` bytes) so mmap integration is
mechanical when the runtime auto-enable slice lands.

### Lock protocol

1. On open, the writer claims ownership of the header. If the magic is
   absent or the checksum fails, the substrate is reinitialised in place
   (`HealedCorruptHeader`). If the magic is valid, the existing
   `owner_pid` is probed.
2. Liveness probe: on unix, `kill(pid, 0)` distinguishes a live owner
   (`AttachedToLiveOwner`) from a dead one (`RecoveredFromCrash`). On
   non-unix targets the probe currently assumes liveness — full crash
   recovery on those platforms is a follow-up.
3. A crashed takeover bumps `generation`, clears `reader_count`, and
   rewrites the header under `sync_data` before the open path is allowed
   to return. The bumped generation is the canonical "ownership changed"
   signal for any observer cached against a prior generation.
4. Embedded readers attach by incrementing `reader_count` and detach by
   decrementing it (saturating). The count survives a writer crash so
   the next opener sees how many stale handles must be cleaned in the
   eventual mmap-backed slice.

### Non-goals (this slice)

- Mapping the file with `memmap2` and wiring readers to share state via
  the mmap region is deferred. The on-disk substrate is the contract;
  the mmap step is mechanical.
- Tier-driven auto-enable (`standard` / `performance` / `max` flipping
  the toggle on at startup) lands with the same `RuntimeOptions` /
  layout wiring that gates gh-471/472/473.

## Folded pager meta — page 0/1 layout (gh-477)

The pager's metadata page lives at page 1 of the data file. Historically
that page was also mirrored to a `<data>-meta` sidecar shadow used as a
last-resort corruption recovery path, and the page was capped at a single
4 KiB frame — large catalogs (many collections, many cross-refs) were
silently truncated when the serialised blob exceeded one page.

The `fold_pager_meta` process-global policy (off by default, env escape
hatch `REDDB_FOLD_PAGER_META=1`) folds the pager meta into the datafile:

- when **ON**, the `<data>-meta` shadow is not written and any pre-existing
  shadow is removed on the next write — page 1 (plus its overflow chain
  when needed) is the sole source of truth;
- when **OFF**, the legacy shadow behaviour is preserved verbatim;
- reads always tolerate either layout, so databases written under the old
  shadow remain loadable after the flag is flipped on.

### Page 0 (database header)

Unchanged. The header carries `freelist_head` (first trunk page id of the
freelist chain) and the mirrored `PhysicalFileHeader` block. See
`crates/reddb-server/src/storage/engine/pager.rs` for the byte layout.

### Page 1 (pager metadata) — single-page form

When the serialised metadata payload fits within `PAGE_SIZE - HEADER_SIZE`
(4064 bytes), page 1 carries the payload directly at content offset 0:

```text
[0..4]   "RDM2"             // METADATA_MAGIC
[4..8]   format_version u32
[8..12]  collection_count u32
…
```

Byte-identical to the historical layout — older readers keep working.

### Page 1 (pager metadata) — overflow form

When the payload exceeds the single-page bound, page 1 switches to a
wrapper header pointing at an overflow chain of `PageType::Overflow`
pages:

```text
Page 1 (content offset):
  [0..4]   "RDM3"                 // METADATA_OVERFLOW_MAGIC
  [4..8]   format_version u32
  [8..12]  total_payload_bytes u32
  [12..16] next_overflow_page_id u32   (> 0)
  [16..]   first payload chunk (≤ 4048 bytes)

Overflow continuation page (content offset):
  [0..4]   next_overflow_page_id u32   (0 = last)
  [4..8]   chunk_bytes u32
  [8..]    payload chunk (≤ 4056 bytes)
```

`N` per-chunk capacity:

- first chunk on page 1: **4048** bytes
- subsequent overflow pages: **4056** bytes

The reader transparently follows the chain; the parser sees a flat byte
sequence identical to the single-page payload, so the upstream parsing
logic is unchanged.

### Free list overflow (page allocation)

The free page list is stored as a linked chain of `FreelistTrunk` pages
rooted at `header.freelist_head` (see
`crates/reddb-server/src/storage/engine/freelist.rs`). A single trunk page
holds `FREE_IDS_PER_TRUNK = 1014` free page ids. When more pages are
freed, additional trunk pages are allocated and threaded through the
chain — there is no static cap. The existing
`freelist::tests::test_trunk_chain` covers > 2000 freed pages and the new
`tests/e2e_fold_pager_meta_policy.rs::freelist_trunk_chain_handles_many_pages`
extends coverage to multi-page reload.

## Fold DWB into WAL — full-page-image records (gh-478)

The legacy `<data>-dwb` sidecar exists solely to recover torn page
writes: pages flush through a staging buffer that is fsync'd before
the in-place write, so a crash mid-write can be healed by replaying
the staging copy. The sidecar costs a second random write per
checkpoint and a third file in the per-database artifact set.

The `fold_dwb_into_wal` process-global policy (off by default, env
escape hatch `REDDB_FOLD_DWB_INTO_WAL=1`) collapses that recovery
contract into the WAL via a new **FullPageImage (FPI)** record. When
ON:

- The pager does not open `<data>-dwb`. Any pre-existing sidecar is
  removed at open time so a flipped flag does not leave a stale
  artifact on disk.
- Recovery applies FPI records before normal redo: every page id
  with an FPI in the active WAL prefix is overwritten with the
  recorded image, then `PageWrite` / `PageWriteCompressed` redo
  proceeds. A torn page that received only part of its bytes is
  therefore healed by the FPI that preceded the first modification
  in the active checkpoint cycle.

### WAL record format (v2, type byte `8`)

```text
[Type: 1 = FullPageImage]
[TxID: 8]
[PageID: 4]
[CkptEpoch: 8]
[DataLen: 4]
[Data: N]            (typically PAGE_SIZE = 4096 bytes)
[CRC: 4]
```

`CkptEpoch` is the checkpoint cycle counter at the time the image
was captured. Recovery resolves duplicate FPIs per page id by
preferring the highest LSN observed; the epoch field is reserved
for future "FPI required" semantics where checkpoint advancement
retires older images.

### Scope of this slice (gh-478)

This slice lands:

- record format + encode/decode + roundtrip tests
  (`crates/reddb-server/src/storage/wal/record.rs`);
- `WalReader::collect_full_page_images` recovery helper
  (`crates/reddb-server/src/storage/wal/reader.rs`);
- `fold_dwb_into_wal` toggle + `-dwb` suppression in the pager
  (`crates/reddb-server/src/physical.rs`,
  `crates/reddb-server/src/storage/engine/pager/impl.rs`);
- `tests/e2e_fold_dwb_into_wal_policy.rs` covering OFF default,
  ON-flip removes the sidecar, and FPI roundtrip via WAL file.

Deferred (tracked alongside #471/#472/#473/#475/#477):

- automatic FPI emission on first page modification per checkpoint
  cycle inside the pager flush path (currently `write_pages_through_dwb`
  is the FPI emission point and stays unchanged until tier wiring
  picks a single path);
- tier-driven auto-enable from `RuntimeOptions`;
- benchmark gate documenting OLTP overhead.

## Promotion criteria for tier-default feature flags (gh-480)

A feature flag may be **promoted** from "off by default + env hatch" to
an ON default on a given tier (`minimal` / `standard` / `performance` /
`max`) only when every objective gate below has been cleared. The
intent is to make tier-default changes boring and auditable — never a
judgement call from a single reviewer.

### Gates

A promotion proposal must point at concrete evidence for each item:

1. **One full release** has shipped with the flag available as a
   voluntary opt-in (env hatch and/or `LayoutOverrides`). The flag's
   landing slice does **not** count as that release — the release that
   landed the flag is the floor, the next tagged release with the flag
   present is the earliest eligible promotion point.
2. **No perf regression on the standard benchmark set** between the
   release that landed the flag and the proposed promotion release,
   measured with the flag ON. The benchmark set is whatever
   `cargo test --release --test *_bench` covers at the time of the
   proposal (e.g. `tests/fold_dwb_into_wal_bench.rs` is the
   load-bearing example for `fold_dwb_into_wal`). Regression is
   defined per-benchmark by the gate the benchmark file itself
   asserts; if a benchmark has no gate, that benchmark is not
   evidence — add one before proposing promotion.
3. **No open `priority:urgent` or `type:incident` issue** against the
   flag's behaviour or any surface it touches. A `type:bug` that is
   not flag-attributed is not blocking unless triage links it.
4. **An ADR addendum** under this ADR records the promotion decision:
   which flag, which tier, the release before/after the promotion,
   and the benchmark numbers cited. Promotion without an addendum
   entry is not promotion — it's drift.

### Mechanics

- The promotion lands by editing `RedDBOptions::apply_tier_defaults`
  in `crates/reddb-server/src/api.rs` so the per-tier truth table
  matches the new default. The doc-comment table in
  `apply_tier_defaults` and the table in `tests/e2e_tier_wiring.rs`
  must move in lockstep — those two tables are the contract.
- The env hatch and `LayoutOverrides` entry that disabled the flag
  before promotion **must remain** for **at least two further
  releases** after promotion lands. That gives operators a documented
  rollback path during the post-promotion observation window. Only
  after those two releases — and only if the flag has accumulated no
  rollback reports — may the override surface be removed in a
  separate deprecation slice.
- Release notes for the promotion release call the new default out by
  name, link to this ADR section, and document the override
  (`REDDB_<FLAG>=0` or `LayoutOverrides { ... }`) the operator can
  set to keep the old behaviour on. Without that operator-facing
  note, the promotion is incomplete.

### What is **not** a promotion criterion

- "It feels mature." Subjective maturity is not a gate; the
  release-count + benchmark + incident triad above is.
- "The author of the flag thinks it's ready." Self-review does not
  satisfy gate 3 — only the absence of incident-class issues from
  the wider tracker does.
- "Other databases default to it." Comparative norms are useful
  context for an ADR addendum but do not substitute for the gates.

### Phase grouping (gh-480)

The `gh-480` meta tracks three phase buckets of candidate flags so
the gates above are evaluated in batches that share an evidence
window, not one PR per flag:

- **Phase A — cosmetic** (low blast radius, observable but
  reversible): tier-default routing of cache and log artefacts into
  `<dbname>.rdb.red/cache/` and `<dbname>.rdb.red/logs/`. This
  phase is already the live default for `performance` and `max`
  (see the `default_audit_log_in` / `default_slow_log_in` table
  above). The promotion was landed under gh-471 and is recorded
  here as the worked example of how the gates apply.
- **Phase B — pager substrate**: `fold_pager_meta`,
  `fold_dwb_into_wal`, and `-shm` provisioning. `-shm` was
  promoted to `standard` under gh-475 (see the tier table in
  `tests/e2e_tier_wiring.rs`). `fold_pager_meta` and
  `fold_dwb_into_wal` remain `max`-only pending gate 1 (a full
  release with the flag available as opt-in) and gate 2 (the OLTP
  overhead benchmark in `tests/fold_dwb_into_wal_bench.rs` is in
  place; the `fold_pager_meta` side needs a paired benchmark
  before that flag is eligible).
- **Phase C — catalog placement**: `embed_catalog_in_datafile` is
  named here as a forward placeholder for the catalog-into-page-1
  consolidation tracked elsewhere on the PRD. The flag has not
  been introduced yet; it cannot be promoted until it exists and
  has cleared the gates above. Any promotion proposal that names
  a flag not present in the codebase is rejected at the ADR-edit
  step.
