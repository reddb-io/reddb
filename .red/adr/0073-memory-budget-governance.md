# Memory Budget Governance

Status: accepted
Date: 2026-07-08

RedDB becomes **system-aware about memory**: every process runs under an explicit
memory budget, detected from the host or declared by the operator at boot, with
the large storage structures pre-sized from it and the ceiling enforced
didactically at runtime. This is the engine-level answer to the scale contracts
in ADR 0038 (embedded working set ≤ budget; serverless strict boundedness) and
the durable fix for the operational OOM class we already live with (in-RAM
structures whose growth is O(data), e.g. the retired spatial R-tree, PRD #1574).

This ADR fixes **governance and invariants**. Concrete cache topology (e.g. an
L1 hot-page tier + larger L2 tier over WAL/file) is deliberately NOT prescribed
here — it is a performance hypothesis and goes through a measured PRD on the
Criterion lane, inheriting these invariants.

## Decisions

### 1. One budget, resolved at boot

Precedence: explicit operator configuration > deployment-profile default
(serverless profiles ship strict defaults) > host detection (cgroup/container
limit first, then physical RAM, with a conservative fraction). The resolved
budget is logged at boot and visible in the stats surface. There is no
"unlimited" mode; the absence of configuration means the detected default, not
infinity.

### 2. Big structures are pre-sized from the budget

Page cache, entity/segment arenas, index memory, and WAL buffers receive
budget shares at startup and pre-allocate their slot structures (fixed page
size → fixed-size slots → arena allocation). This is the TigerStyle spirit
already adopted by ADR 0056 ("don't allocate per operation — pool and reuse"),
now with a number attached. It is NOT TigerBeetle's literal static-allocation
rule, which ADR 0056 rejected and stays rejected: dynamic structures remain
legal outside hot paths; what changes is that their growth is accounted
against the budget.

### 3. Hot paths allocate zero per operation (ratchet)

Read/write/scan hot paths must not malloc per operation; buffers come from
pools sized in decision 2. Enforced as a ratchet on new/changed code in the
storage data plane (same mechanism as the `.unwrap()` ban), not a big-bang
rewrite.

### 4. Enforcement is didactic, never an OOM kill

An operation whose admission would exceed the budget fails with a named limit
and the current accounting ("operation needs ~X over budget Y; largest
consumers: …"), or triggers reclamation (decision 5) when reclamation can
satisfy it. The process never relies on the kernel OOM killer as its limit
mechanism. This extends the no-silent-zeros posture from the query surface to
resource limits.

### 5. Consolidation is the reclamation tool

Sealed-segment consolidation — merging sealed segments and garbage-collecting
tombstones — is a first-class, budget-driven mechanism: it runs when tombstone
or fragmentation ratios cross thresholds, returning memory to the budget. Per
ADR 0038 §3 it is paced (bounded per-tick cost), never bursty. This replaces
the vestigial LSM scaffolding in the unified layer: the unused write-buffer
memtable is DELETED (a RAM-resident store has no disk latency to amortize, so
it buffered nothing), and the stub compaction hook either becomes this
mechanism or is deleted with it — no aspirational code remains.

### 6. Recursion is banned in the storage/recovery data plane

Amendment to ADR 0056's reframed rule: inside the storage engine's data plane
and every recovery path, recursion is prohibited outright (explicit stacks
with capacity from the budget), because a stack overflow during recovery is a
data-loss event, not a crash. Parser/planner/query surfaces keep the existing
depth-bounded-recursion rule unchanged.

## Considered Options

- **Literal TigerBeetle static allocation** — rejected (unchanged from ADR
  0056): arbitrary collections, variable rows, and RQL plans make it fight
  the domain; the budget delivers the predictability without the freeze.
- **Best-effort soft limits (log a warning, keep allocating)** — rejected:
  a warning at 110% is an OOM kill at 130%; the limit must gate admission.
- **Per-subsystem hard caps with no shared budget** — rejected: caps that
  don't share one accounting pool re-create the OOM by summation (each
  subsystem individually "within limits", the process dead).
- **Prescribing the L1/L2 cache topology in this ADR** — rejected: topology
  is a benchmark question (Criterion lane), and prose-first cache design is
  the aspirational-doc disease this review is curing.

## Consequences

- ADR 0038 phase 4 (server-profile disk residency) builds on this: the paging
  hierarchy that makes dataset > RAM possible is governed by the same budget.
- A follow-up PRD designs the tiered cache (L1 hot pages / L2 / file) with
  Criterion evidence; it inherits decisions 2–4 as constraints.
- The unified-layer cleanup (delete memtable, wire-or-delete the compaction
  stub into consolidation) is immediate executable work, sliceable now.
- `red.stats` grows a budget section (resolved budget, per-pool shares, live
  accounting, reclamation counters) so enforcement is observable.
- ADR 0056 gains a one-line pointer to decision 6 (recursion hardening) so
  the style doc and this ADR cannot drift.
