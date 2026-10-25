# Storage Packaging by Profile — Single-file Zoned RDB and Its Siblings

Status: accepted
Date: 2026-07-08 (rewritten; originally proposed as "Embedded Single-file Zoned RDB");
amended 2026-07-10 (§5: WAL region sizing presets + overflow policy)

RedDB's physical storage packaging is a **per-profile contract**. This ADR is the
umbrella: it names each profile's packaging and scale contract, fixes the zoned
single-file layout as the embedded target, and — the part the original version
lacked — attaches **phases with auditable exit criteria** so the distance between
the promised layout and the shipped sidecars is tracked, never assumed.

The 2026-07-08 rewrite was triggered by an architecture review against Cassandra
and TigerBeetle which found the original ADR aspirational: `Status: proposed`
while `reddb-file/src/layout.rs` carries ~15 sidecar extensions that ARE the
shipped reality. An ADR whose status diverges from the code misleads exactly the
way a dead syntax does; this rewrite applies the same remedy (a plan with owners
and exit criteria, or an honest status).

## Decisions

### 1. Packaging and scale are contracted per profile

- **Embedded (standalone)** — one `.rdb` file as the operator-visible durable
  artifact: create, copy, move, back up, delete one file. **Scale contract:
  working set ≤ the memory budget (ADR 0073).** The budget is detected or
  declared at boot and enforced didactically; exceeding it is an operating
  error with a named limit, never an OOM kill.
- **Server** — same zoned grid, plus the **disk-resident roadmap**: zones for
  entity/segment state that exceed RAM, paged through the budget-governed
  cache hierarchy. Dataset > RAM is a committed direction for this profile
  only; it is explicitly NOT promised for embedded.
- **Serverless** — bounded-everything posture: strict memory/CPU budget
  (ADR 0073), fast-boot snapshot/segment packaging (see `context/serverless.md`),
  cold-start-aware layout. Boundedness here is a survival contract (billing,
  host kill), not hygiene.
- **Primary-replica / cluster** — may choose directory or segmented layouts
  when boot speed, replication streaming, snapshot distribution, or repair
  make them more appropriate. The single-file contract binds embedded, not
  the fleet.

### 2. The embedded single file is zoned internally

Unchanged from the original decision: explicit zones for superblock copies
(ping-pong pair with generation + checksum), internal manifest/catalog, a
circular WAL region, page/grid storage, free-space metadata, checksums, and
overflow/blob extents. SQLite-like to users, TigerBeetle-like inside. The
embedded manifest is internal and authoritative; sidecars are not part of the
promoted embedded contract.

### 3. Maintenance is paced, never bursty

All storage maintenance — segment consolidation (ADR 0073), scrub (ADR 0074),
future append-only compaction (#1808 lane), archival — runs as **incremental,
tick-paced work with a bounded per-tick cost**. Background maintenance threads
with unbounded bursts (the Cassandra compaction model) are prohibited across
every profile. Rationale: bursty maintenance destroys p99 on embedded, billing
on serverless, and replica lag on the fleet; the TigerBeetle-style paced model
is strictly more predictable and composes with the memory/CPU budget.

2026-10-25 proposed exception — local paged CLI recovery WAL. Version 1.26.8
retains a 131,931,435-byte unified WAL after 100 whole-table updates of 1,000
512-byte rows and 65 seconds idle. An explicit checkpoint drains its logical
length to the 8-byte header in 770 ms. The existing CLI periodic checkpoint
requires remote-backup configuration, so a default local server never drains it.
This is separate from physical tree retirement and MVCC history reclamation.

Until checkpoint publication is incremental, the CLI proposes one owned local
checkpoint worker for actual paged, writable, standalone stores without a remote
backend. It observes logical disk WAL length once per second, skips empty WAL,
and requests the existing atomic checkpoint at 16 MiB pressure or 60 seconds
of observed dirty age. There is at least 30 seconds between completion of an
attempt and the next attempt, including failures. Replication, remote archival,
embedded single-file stores and SDK-only runtimes retain their existing policy.

This **does contradict the bounded per-tick work rule above**: rate-limited full
checkpoints are not incremental checkpoints. It is an explicit transitional
proposal, not a claim that the architecture requirement has been met. The
existing publication fence, WAL sync/truncation order and recovery format are
unchanged. Failed checkpoints retain their recovery WAL. No external truncation
or hard disk-space guarantee is introduced. A single large transaction, slower
storage, long-lived transaction or checkpoint failure can exceed the trigger.

Cost sketch: a 14.675 GB historical Auth bundle (68.6 MB encoded physical payload)
needed 5.19–6.73 seconds per warm checkpoint and 9.10 seconds cold under a 2 GiB
cgroup / 1 GiB engine budget, with concurrent query p99 22 ms and peak memory
1.67 GB. At a 60-second dirty cadence that can occupy roughly 9–15% of one
checkpoint worker. These are one frozen Auth copy, not a bound for other stores;
writer latency and the larger Platform copy still require qualification before
fleet activation. The smaller local reproduction is the before/after regression
workload. Incremental publication remains required to remove this exception.

### 4. Phases with exit criteria (the honesty mechanism)

Each sidecar family retires only when its phase's exit criteria hold. A phase
is DONE when: (a) the promoted embedded profile creates **no** sidecar of that
family on a fresh store, (b) the in-file zone replacing it is covered by a DST
crash campaign with the `recover_and_check` oracle, and (c) `layout.rs` drops
the extension (clean break — no dual-read window beyond one minor release).

| Phase | Scope | Retires |
|---|---|---|
| 0 (now) | This rewrite: sidecar reality is documented as transitional, tracked here | — |
| 1 | Superblock ping-pong pair + internal manifest in-file | `rdb-hdr`, `rdb-meta` (+ shadows) |
| 2 | Circular WAL region in-file | `rdb-uwal`, `redwal`, `rdb-wal`, legacy `wal` |
| 3 | Double-write/recovery state in-file | `rdb-dwb` (+ shadow), `shm` review |
| 4 | Server-profile disk-resident entity/segment zones (dataset > RAM) | — (new capability, not a retirement) |

Phase 3 amendment: embedded keeps `shm` retired/absent. The `shm` file is
coordination state, not durable state, and the embedded single-file contract
does not need a sibling file to recover bytes. Profiles that require
multi-process coordination may still provision `shm` through their explicit
tier policy, but that is outside the embedded packaging contract and is asserted
separately from the DWB sidecar census.

Phase 3 amendment: in-file DWB zone interpretation is gated by the paged-file
version marker introduced with phase 3. Older stores route through the offline
migration tool rather than treating pages 3-66 as a DWB zone.

Ordering within phases 1–3 may be re-sequenced by an ADR amendment with one
line of rationale; silently skipping a phase is not allowed. Phase 4 is gated
on ADR 0073 landing first (the cache hierarchy it pages through is
budget-governed).

### 5. WAL region sizing and overflow are per-profile presets

The circular WAL region (phase 2) is sized by the deployment profile, not by
one global constant. Initial sizes and growth are presets; the values below
are the starting defaults and may be retuned by a one-line amendment with a
measurement attached.

| Profile | Initial region | Growth | Cap |
|---|---|---|---|
| Embedded | 64 KiB | doubles on demand | none (disk-bound) |
| Serverless | 64 KiB | doubles on demand | 1 MiB (bounded-everything contract) |
| Primary-replica | 16 MiB | doubles on demand | none |
| Cluster | follows primary-replica until measured otherwise | | |

Overflow policy, in order, when an append does not fit the free span:

1. **Checkpoint-then-retry** — if the region holds reclaimable bytes (records
   behind the checkpoint fence), advance the reclamation fence and retry the
   append once. This is the common case and must not error.
2. **Grow at the fence** — if the record still does not fit and the profile
   preset allows growth, resize the region at a checkpoint fence (region
   quiesced/empty), doubling until the record fits or the cap is reached.
   The snapshot zone relocates behind the grown region, as today.
3. **Didactic error** — only when a single record cannot fit an EMPTY region
   at the profile's cap. The error names the record size, the region size,
   the cap, and the profile, and points at the preset. Never a silent
   truncation, never a partial append.

Sizing rationale: 64 KiB keeps a fresh embedded/serverless store small on
disk; 16 MiB for the fleet follows the industry reference points (SQLite
auto-checkpoints at ~4 MiB of WAL; Postgres segments are 16 MiB) where boot
footprint matters less than checkpoint pressure under sustained writes.

2026-10-21 amendment — normal embedded overflow checkpoints request headroom
proportional to the snapshot image, with an adaptive target capped at 1 MiB,
rounded up by the existing power-of-two capacity allocator. Fresh artifacts
still start at 64 KiB; existing larger regions are not shrunk. An indivisible
larger action retains the existing embedded oversize contract. This bounds
normal additional disk/replay reservation, not the size of a valid record.
The populated-store diagnostic measured 731.88 ms grouped-write p99 and only
205 writes/s; sizing only for one small failed action left the reservation
at 64 KiB and repeatedly checkpointed the whole image. Retry under the
publication fence now revalidates stale full results before checkpointing.
Both sync barriers and on-disk geometry rules are retained. The long proving
workload records actual snapshot changes across multiple WAL rollovers,
throughput, p99 and crash recovery under `bench/results/kv-compare-wal-2026-10-21`.
That five-by-10,000-write, eight-connection diagnostic measured grouped
512.6 -> 817.8 writes/s and p99 163.67 -> 22.62 ms; Strict measured
138.0 -> 184.7 writes/s and p99 328.78 -> 209.50 ms. Snapshot images changed
in every round and all four cases recovered after SIGKILL. The fixed-order
shared-host study has no independent-process confidence interval; Strict's
worst pause increased from 6.24 to 6.65 seconds. Full ranges and unfavorable
samples are retained in the capture's `SUMMARY.md`.
This does not introduce a background maintenance worker or claim incremental
checkpointing; paced incremental maintenance remains the architectural target.

## Considered Options

- **Single zoned `.rdb` for embedded** — kept (unchanged rationale: ergonomics
  + formal recovery room).
- **Sidecars as the permanent embedded contract** — rejected again; weakens
  copy/delete/backup ergonomics and multiplies partial-file failure modes.
- **One packaging for all profiles** — rejected; replication/serverless have
  structurally different boot/streaming needs (original ADR already carved
  this out; the rewrite makes each profile's contract explicit).
- **Keeping the ADR `proposed` with no phase plan** — rejected by this rewrite;
  status must either be executable or say "vision".

## Consequences

- ADR 0073 (memory budget) and ADR 0074 (storage fault model) are companions:
  0073 owns the scale contract enforcement, 0074 owns per-zone corruption
  behavior. This ADR owns packaging and pacing.
- Embedded open/recovery must validate superblock generations/checksums, load
  the internal manifest, and replay the internal WAL region from the
  checkpoint boundary (unchanged).
- Every phase completion is provable in CI: fresh-store sidecar census + DST
  campaign green. "0 sidecars created" is asserted, not eyeballed.
- Migration must distinguish legacy sidecar-backed stores from zoned `.rdb`
  stores; per the house no-backcompat posture, a major zoned bump reads the
  old form only through the explicit offline migration tool, never silently.
