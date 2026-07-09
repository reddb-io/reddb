# Storage Fault Model — Detect, Scrub, Salvage; Paranoid Cluster

Status: accepted
Date: 2026-07-08

RedDB already pays for detection (page-header checksums, superblock
generation+checksum pairs, checksummed recovery boundaries — ADR 0038) and
already owns the right proving harness (DST with fault injection via
`unreliable-libc` and the `recover_and_check` oracle). What has never been
written down is the **contract after detection**: which physical faults we
model, what each zone guarantees when corruption is found, and what the
operator can do about it. TigerBeetle detects *and repairs from replicas*;
single-node RedDB cannot repair from a replica, so its honest contract must be
explicit rather than implied.

## Decisions

### 1. The modeled fault classes

We explicitly model, and DST must exercise: torn writes (partial page/sector),
misdirected writes (right data, wrong offset), bit rot (checksum mismatch on
read), lost writes (fsync acknowledged, data absent), and crash at any point
(already covered). Anything outside this list (RAM corruption, kernel page
cache lying beyond fsync semantics) is out of model and documented as such.

### 2. Per-zone detection contract (embedded)

- **Superblock**: two ping-pong copies; open picks the newest valid one. Both
  invalid → the store does not open; salvage (decision 4) is the path.
- **Manifest/catalog**: checksummed; corruption fails open with a didactic
  error naming the zone.
- **WAL region**: recovery stops at the first invalid record — a torn tail is
  normal crash truncation (recover to last valid), while corruption BEFORE the
  checkpoint boundary is reported as corruption, never silently truncated.
- **Pages/blocks**: checksum verified on read; a failing page fails the READ
  didactically (named page, named collection when derivable) — it never
  returns garbage rows.

### 3. Scrub is a first-class embedded tool

An operator-invocable (and paceable background, per ADR 0038 §3) integrity
pass that verifies every checksum — superblocks, manifest, WAL region, all
reachable pages/blocks — and reports a structured result. The SQLite
benchmark is `PRAGMA integrity_check`; ours must additionally distinguish the
fault classes of decision 1 where the evidence allows. Scrub findings feed
`red.stats` and never mutate the store.

### 4. Salvage is a first-class embedded tool

A best-effort extractor that reads a damaged `.rdb` and exports every
recoverable entity, skipping (and enumerating) what fails verification — the
`.recover` of SQLite, DST-proven: campaigns corrupt stores with decision-1
faults and assert salvage recovers everything the faults did not touch.
Salvage never writes into the damaged file; it produces a fresh store plus a
loss report.

### 5. The cluster profile is paranoid — mandatory, not best-effort

When the cluster/replica profiles come out of hold, this contract binds them:
a node NEVER serves data whose checksum failed (fail the read, fetch from a
peer); scrub runs continuously (paced); **repair-from-replica is a mandatory
capability of the profile**, not an optimization — a node that detects local
corruption heals from peers or evicts itself. Design reviews for replication
work must cite this section.

### 6. Determinism is test-scoped, by declaration

Execution determinism (simulated clock, seeded randomness, `buggify!` fault
points) is a property of the DST harness, NOT a runtime guarantee. Replication
copies bytes (WAL/log shipping); deterministic state-machine replication in
the TigerBeetle/VSR sense is **out of horizon** — production code is free to
use wall clocks, hash iteration order, and thread scheduling, and no reviewer
should reject such code "to keep SMR possible". Reversing this requires a new
ADR, not an assumption. (Bounded/predictable behavior — pre-allocation, paced
maintenance — is governed by ADR 0073/0038 and is orthogonal to determinism.)

## Considered Options

- **Detect + fail + restore-from-backup only** — rejected as the whole story:
  cheapest, but an embedded database without scrub/salvage tooling fails the
  operator exactly when they need it most (the SQLite bar exists for a reason).
- **Full local self-repair (redundant zones beyond the superblock, WAL-history
  page repair)** — rejected for embedded: buys resilience the cluster profile
  will get structurally (peers), at high complexity now.
- **Leaving determinism scope undeclared** — rejected: undecided architecture
  questions fossilize into aspirational half-code (this review found three
  such cases); the declaration is one paragraph and reversible by ADR.

## Consequences

- Scrub and salvage become PRD-able units with DST campaigns as their
  acceptance tests; the fault classes of decision 1 become named `buggify!`
  points in `unreliable-libc`.
- The read path grows the didactic checksum-failure error (named zone/page),
  replacing whatever implicit behavior exists today.
- Replication design docs must carry a "decision 5 compliance" section when
  that work resumes.
- ADR 0038's phase exit criteria implicitly reference this ADR: an in-file
  zone is not "done" until its decision-2 contract is DST-proven.
