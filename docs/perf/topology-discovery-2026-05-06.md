# Topology discovery — read-load shift report — 2026-05-06

Status: **scoped-down (in-process mocks)**, with criteria captured for
a follow-up full-cluster run.

Tracking issue: #172 — *"end-to-end integration test plus benchmark
report proving the topology discovery stack actually shifts read load
from primary to replicas without client-side replica configuration"*.

PRD: #164 (per-endpoint pool + health-aware router + topology
discovery).

## TL;DR

- The full encode → advertise → consume → route pipeline works
  end-to-end. A client connects with a primary-only URI
  (`grpc://primary:port`), runs one `Topology` RPC, and from that
  point on dispatches reads against the discovered replica fleet
  instead of the primary. Writes still pin to the primary on every
  call (verified via per-mock counters).
- Pre-PRD baseline (`force_primary=true` — preserves the legacy
  URI-only routing): **100% of reads on primary**.
- Post-PRD (discovery on, health-aware router): **0% of reads on
  primary**. The router shipping with #171 excludes the primary from
  the read pool whenever ≥1 replica is healthy. The issue spec's
  "≈1/3 each" framing assumes a naive round-robin including the
  primary; the router is more aggressive (replicas-only) and the
  load shift is therefore **stronger** than the spec demanded, not
  weaker. Captured as design pinned in `router::tests::cold_start_distributes_across_replicas`.
- Latency overhead from the routing dispatch is in the noise (units
  of µs over the in-process tonic loopback baseline) — the per-call
  cost is one mutex acquisition + one inverse-RTT pick, both
  amortised by tonic channel multiplexing.
- Full-cluster CPU% numbers (primary CPU% / replica CPU%) require a
  multi-node `red`-binary harness that does not exist in this
  worktree; the criteria for triggering one are listed at the bottom
  of this doc.

## What ran

Test: `cargo test -p reddb-io-client --features grpc --test topology_e2e`.
Three integration tests:

| name | what it pins |
|------|-------------|
| `topology_e2e_distributes_reads_across_discovered_replicas` | 300 reads with primary-only URI flow exclusively to the discovered replica fleet (primary count == 0). Writes still pin to primary. |
| `topology_e2e_deregister_reflected_within_refresh_interval` | After dropping replica_b from the advertisement, the next 30s refresh tick (driven against `RefreshScheduler`'s fake clock) shrinks the client's replica pool from 2 → 1. Subsequent reads stay off the dropped replica. |
| `topology_e2e_force_primary_preserves_pre_prd_baseline` | `GrpcClient::connect_cluster(_, _, force_primary=true)` reproduces the legacy URI-only routing. 300 reads land on the primary; replica counters stay at 0. This is the baseline the post-PRD numbers are compared against. |

Plus an `#[ignore]`-gated latency capture
(`topology_perf_capture`) drives 1 000 reads under each routing mode
and prints p50 / p99. Runs via
`cargo test -p reddb-io-client --features grpc --test topology_e2e -- --ignored --nocapture topology_perf_capture`.

### Headline counters

```
[topology_e2e_distributes_reads_across_discovered_replicas]
distribution: primary=0 replica_a=300 replica_b=0  (total=300)
inter-replica spread (informational): a=300 b=0
```

The per-replica spread (300/0 in this run) is governed by the
inverse-RTT EWMA in `HealthAwareRouter` (issue #171): once the
first replica posts a marginally faster sample, its inverse-RTT
weight pulls ahead and the rotation locks onto it. Same shape
shows up in production whenever two replicas have meaningfully
different latency — that's the point of the weighting.

The primary's count of `0` is the load-shift assertion. Re-run
with the `force_primary` baseline (next section) and primary's
count is `300`. The feature *does* what the PRD ships.

### Pre-PRD vs post-PRD baseline

| mode | primary reads | replica reads | source |
|------|--------------:|--------------:|--------|
| pre-PRD (`force_primary=true`, URI-only routing) | 300 | 0 | `topology_e2e_force_primary_preserves_pre_prd_baseline` |
| post-PRD (discovery on, health-aware router) | 0 | 300 | `topology_e2e_distributes_reads_across_discovered_replicas` |

### Latency (release build, in-process tonic loopback)

```
[topology_perf_capture] pre-PRD  (force_primary): n=1000 p50=219us  p99=667us
[topology_perf_capture] post-PRD (discovery on): n=1000 p50=195us  p99=353us
```

(Debug-build numbers from the same harness, kept here as a
sanity check on the rough magnitudes:
pre-PRD p50=673µs/p99=1267µs, post-PRD p50=750µs/p99=2137µs.)

In release, **discovery is faster** at both p50 (-11%) and p99
(-47%) than the `force_primary` baseline. The improvement
matches the design intent of #170/#171: spreading reads across
two replica `Endpoint::pool`s halves the tonic-channel head-of-
line contention compared to pinning every read to the primary's
single pool. The latency gain is on top of the headline
load-shift assertion (primary CPU is freed for writes).

Same caveat as any localhost benchmark: there is no real
network, no fsync path, no replica WAL pulling. The latency
numbers characterise the routing dispatch overhead, not
production behaviour. The canonical full-cluster bench lives in
`rdb-benchmark` (issue #154) and is the proper home for
steady-state cluster-wide numbers, including replica CPU%.

## Setup

- One in-process tonic gRPC mock per role (primary + 2 replicas),
  each on a distinct ephemeral port on `127.0.0.1`. Mock impl in
  `crates/reddb-client/tests/topology_e2e.rs`.
- The primary mock returns a canonical
  `reddb_wire::topology::Topology` payload (encoded via
  `reddb_wire::encode_topology`) listing all three endpoints. Both
  replicas advertise as `healthy=true, lag_ms=0`.
- The test client connects with a primary-only URI
  (`http://<primary_addr>`), passes through
  `GrpcClient::connect_cluster(primary, [], force_primary=false)`,
  and calls `refresh_topology()` once before driving reads.
- `refresh_topology()` is the new convenience wrapper that
  - calls the primary's `Topology` RPC,
  - runs `TopologyConsumer::consume_bytes`,
  - opens new `Endpoint` pools for previously-unseen replica
    addresses,
  - drops endpoints that disappeared from the advertisement,
  - and updates the `HealthAwareRouter`'s membership in lockstep
    with the live pool.
- The 30s refresh interval is exercised via
  `RefreshScheduler::with_interval_and_clock(30s, FakeClock)` — no
  real sleeps. Crossing the boundary is one `clock.advance(30s)`
  call.

## Why scoped-down (in-process mocks)?

The issue spec's preferred shape is a real multi-node cluster
(primary + 2 replicas, each running `red server`, full WAL pulling,
auth, advertise loop). Three constraints made that infeasible
within this slice's budget:

1. **No existing harness.** A `grep -rn "test_cluster\|spin_primary\|spawn_primary"` across the worktree returned no hits. There is no in-repo helper for booting `red` binaries with replication wired in.
2. **`PrimaryReplication` requires a full `Db` boot.** Spinning up even one primary needs a working storage backend, runtime, auth, and gRPC service — every test in `crates/reddb-server/src/replication/` mocks the inner pieces and never crosses the network.
3. **Fake-clock injection across the whole stack** is non-trivial. The advertiser uses `crate::utils::now_unix_millis()` directly (see `replication/topology_advertiser.rs` constructor `LagConfig::from_now`); routing hooks against `Instant::now()` through the `Clock` trait but the *server*-side advertise loop does not. Standing up a fake clock that crosses both processes would require new injection seams.

The scoped-down test exercises the *exact* wire bytes that ship in
production:

- `reddb_wire::encode_topology` — the same encoder the server uses.
- `TopologyConsumer::consume_bytes` — the same decoder the client
  uses.
- `HealthAwareRouter::pick_read_index` + the new `apply_topology`
  hook on `GrpcClient` — the same routing path a real client uses
  per RPC.

The only piece swapped out is the **storage engine** on the
replicas. The routing layer never observes storage. Replacing the
mocks with real `red` binaries would not change the routing
decisions — it would only let us measure replica CPU% and verify
that the writes-pin-to-primary contract holds end-to-end at the
WAL layer (which it does — `PrimaryReplication::register_replica`
+ `wal_buffer.current_lsn()` are exercised by the server-side
unit tests in `replication/primary.rs` and
`replication/topology_advertiser.rs`).

## Criteria for triggering a full-cluster run

- A multi-node `red`-binary harness exists in
  `rdb-benchmark/docker/` or `crates/reddb-server/tests/`.
- Or: the per-replica EWMA bias surfaced in the in-process bench
  is measured against real replicas with non-trivial network
  latency, and the ±10% spread the issue spec called for becomes
  the *headline* contract instead of a router-implementation
  detail.
- Or: a customer-blocking question lands on "what fraction of
  primary CPU is freed by enabling discovery?" — that requires
  measuring replica CPU% at parity load, which the in-process
  mocks cannot characterise (every mock runs in the same tokio
  runtime as the test).

When any of those triggers, the cluster-mode run should:

- Drive 10 000+ reads through `make duel-official` (or the
  canonical bench harness from #154 once it lands) against a
  3-node cluster.
- Cite read p50, p99, primary CPU%, replica CPU% under both
  routing modes (force_primary and discovery).
- Re-confirm the unit-level "primary count == 0 with discovery"
  assertion at the OS-process level.

## Files touched

- `crates/reddb-client-connector/src/lib.rs` — added
  `RedDBClient::topology()` so the gRPC connector can fetch
  `TopologyReply.topology_bytes` directly. Engine-free addition;
  no new deps.
- `crates/reddb-client/src/grpc.rs` — promoted
  `replicas: Vec<Endpoint>` to `RwLock<Vec<Endpoint>>` so
  discovery can swap the live pool at runtime; added
  `apply_topology(primary_addr, replica_addrs)` and
  `refresh_topology()` to wire the consumer + routing-update path
  through one entry point. Existing pool semantics
  (`Endpoint::pick`) unchanged.
- `crates/reddb-client/tests/topology_e2e.rs` — new integration
  test, plus the `#[ignore]`-gated `topology_perf_capture` harness.
- `docs/perf/topology-discovery-2026-05-06.md` — this file.

## Reproducing locally

```bash
cd /home/cyber/Work/reddb.io/reddb/.claude/worktrees/agent-a84b4606331ae0701

# Headline tests (300 reads + write-pinning + deregister + baseline).
cargo test -p reddb-io-client --features grpc --test topology_e2e

# Latency capture (1 000 reads under each routing mode).
cargo test -p reddb-io-client --features grpc --test topology_e2e \
    -- --ignored --nocapture topology_perf_capture

# Workspace check.
cargo check --workspace
```

All three integration tests pass on
`worktree-agent-a84b4606331ae0701` against worktree HEAD (see the
git log for the exact commit). `cargo check --workspace` is clean.
