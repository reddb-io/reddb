# Cluster Architecture Gap Analysis: RedDB vs Vitess

Date: 2026-06-29

This document compares RedDB's current cluster architecture against Vitess as an
operational benchmark. The goal is not to copy Vitess. Vitess is MySQL-based and
RedDB owns its storage engine. The useful comparison is the maturity of the
cluster product surface: routing, topology, failover, resharding, operator
workflow, and test coverage.

Official Vitess sources used:

- Vitess architecture: https://vitess.io/docs/24.0/overview/architecture/
- VTGate: https://vitess.io/docs/24.0/reference/programs/vtgate/
- Topology Service: https://vitess.io/docs/23.0/reference/features/topology-service/
- Sharding: https://vitess.io/docs/24.0/reference/features/sharding/
- Reshard: https://vitess.io/docs/25.0/reference/vreplication/reshard/
- VTOrc architecture: https://vitess.io/docs/24.0/reference/vtorc/architecture/
- vtctldclient: https://vitess.io/docs/24.0/reference/programs/vtctldclient/

## Executive Summary

RedDB has unusually strong cluster design work for its age: range ownership,
epochs, redirect hints, topology snapshots, failover safety, witness voting,
control-plane/data-plane separation, move-range cutover, Jepsen-style process
testing, and Maelstrom-style protocol modeling are already present as ADRs,
pure modules, or early runtime code.

The maturity gap is that Vitess has turned those ideas into named operational
surfaces. Applications talk to VTGate. Operators inspect and mutate topology
through vtctld/vtctldclient. VTOrc owns automated failure detection and repair.
VReplication workflows expose resharding as create/status/validate/switch/cancel
/complete. The topology service stores consistent metadata and locks while the
serving graph keeps per-query routing off the global topology path.

RedDB's next cluster work should therefore be product-shaped, not just
module-shaped:

1. define the RedDB gateway/router role and driver topology contract;
2. expose a serving graph/topology API that is authoritative, cacheable, and
   independent from the user data path;
3. make failover and range movement first-class operator workflows;
4. wire pure routing, ownership, supervisor, and move-range models into public
   transport tests;
5. keep the current discipline that user writes do not flow through the
   control-plane consensus log.

## Comparison Matrix

| Area | Vitess pattern | RedDB state | Gap |
|---|---|---|---|
| Application gateway | VTGate is a stateless proxy that accepts application traffic and routes to tablets. | Any-node routing and client topology are modeled in `cluster/routing.rs` and `cluster/topology.rs`. | No named gateway/router runtime or public routing API yet. |
| Topology authority | Topology service stores small consistent metadata, locks, watches, keyspaces, shards, tablets, replication graph, and serving graph. It is not on the per-query path. | ADR 0037 and ADR 0052 define a versioned ownership catalog and Raft-equivalent control-plane log. | Need a concrete topology store/snapshot API and a serving graph contract. |
| Serving graph | Per-cell SrvKeyspace and SrvVSchema let vtgates route quickly from local rolled-up state. | `TopologySnapshot`, `TopologyRange`, and redirect hints carry ranges, owners, replicas, epochs, and catalog versions. | Need a stable wire shape, cache policy, watch/push behavior, and stale-route retry contract. |
| Sharding model | Keyspaces are sharded into key ranges; shards usually contain one primary plus replicas. | `docs/architecture/cluster-sharding.md` and ADR 0055 define hash slots, ordered ranges, shard groups, and bounded cross-range behavior. | Strong model, but no public `SHARD BY` DDL or complete serving path. |
| Failover | VTOrc detects failures, coordinates through shard locks, and repairs through tablet RPCs. vtctldclient exposes planned/emergency reparent commands. | Replication failover coordinator, election core, witness profile, and cluster supervisor policy exist as pure or early modules. `docs/deployment/replication.md` still warns automatic failover is future. | Need public planned/emergency failover commands and a real supervisor runtime loop. |
| Resharding/range movement | Reshard is a workflow: create, show/status, VDiff, SwitchTraffic, ReverseTraffic, cancel, complete. | `cluster/move_range.rs`, placement, split, catch-up, and cutover state machines exist as pure models. | Need an operator workflow around move/split/cutover with status, validation, rollback/reverse, and dry-run. |
| Control/data separation | Topology is metadata/locks, not RPC/log storage or per-query dependency. | ADR 0052 explicitly keeps user writes out of the control-plane consensus log. | Preserve this; do not "fix" clustering by pushing data writes through consensus. |
| Operator surface | vtctldclient and VTAdmin expose broad cluster inspection and mutation. | RedDB has status JSON and deployment JSON surfaces plus individual docs. | Need one cluster operator surface with topology, health, ownership, failover, and movement commands. |
| Testing | Vitess documents operational behaviors and has mature workflow surfaces to exercise. | RedDB has Maelstrom-style protocol model and Jepsen-style black-box harness docs, plus chaos replication tests. | Need public workflow E2E tests: planned failover, emergency failover, stale route, move range, and interrupted move. |

## What RedDB Already Gets Right

RedDB's biggest architectural strength is the same separation Vitess relies on:
metadata/control decisions are separated from the per-query data path.
[ADR 0052](../../.red/adr/0052-cluster-supervisor-control-plane-consensus.md)
keeps user-data writes outside the control-plane consensus log. That is the
right boundary to defend.

The ownership catalog is also the right primitive. [ADR 0037](../../.red/adr/0037-shard-range-ownership-catalog.md)
uses versioned range ownership with writer owner, replicas, epoch, and catalog
version. That maps well to Vitess' global shard records and serving graph, while
remaining native to RedDB's range model.

The topology/routing decision layer is thoughtfully constrained. It supports
authoritative polling, advisory redirect hints, optional push updates, epochs,
catalog versions, and owner-side write fencing. That is stronger than a naive
client-side hash ring because correctness does not depend on a fresh client
cache alone.

The range movement model is also pointed in the right direction. Copy,
catch-up, commit-watermark-gated cutover, and interrupted-move recovery are the
right ingredients for Vitess-like resharding without importing MySQL-specific
mechanics.

Finally, the test direction is correct. The Maelstrom-style protocol model and
Jepsen-style process harness are the right low-cost path before paid or
deterministic hypervisor testing. They should now be aimed at the operator
workflows, not only at isolated protocol pieces.

## Highest Priority Gaps

### 1. Name And Expose The Gateway Contract

RedDB needs a named equivalent to VTGate. It does not have to be a separate
binary immediately. It can start as a `red router` profile or an "any-node
gateway" mode inside `red server`, but the product contract needs a name.

Minimum contract:

- accepts ordinary client traffic;
- maintains a topology snapshot cache;
- routes single-shard reads/writes directly to owners;
- returns stale-route redirects with owner, range, epoch, and catalog version;
- buffers or fails predictably during failover;
- exposes metrics for routing cache age, redirects, forwards, stale epochs, and
  owner-fencing rejects.

### 2. Create A Serving Graph API

The current topology snapshot is close to a serving graph, but it is still a Rust
model. RedDB needs a public, versioned API for drivers and routers.

Minimum contract:

- `GET /cluster/topology` or equivalent gRPC/RedWire method;
- cluster generation plus per-range catalog version;
- range bounds or slot spans;
- owner and replica endpoints;
- ownership epoch;
- read eligibility metadata, including lag/freshness where available;
- watch or long-poll option as an accelerator, with polling remaining sufficient
  for correctness.

The API must explicitly say that topology is not a per-query dependency.
Requests remain safe because the owner checks the epoch/fence below routing.

### 3. Productize Planned And Emergency Failover

Vitess has distinct planned and emergency reparent commands. RedDB's
`FailoverCoordinator`, election core, witness profile, and supervisor policy are
close to that shape, but the user-facing workflow is not finished.

Minimum contract:

- `red cluster failover plan --target <node>`;
- `red cluster failover execute --target <node>`;
- `red cluster failover force --target <node> --reason <text>`;
- explicit zero-RPO vs forced outcome;
- surfaced skipped LSNs for forced failover;
- timeline/term/epoch in the result;
- rollback-tail artifacts and operator events when data is discarded;
- E2E test using real `red` processes and public APIs.

### 4. Productize Move Range / Resharding

Vitess' Reshard workflow is more mature because it is an operator workflow, not
just code that can move bytes. RedDB should wrap `move_range.rs` in a workflow
with visible phases.

Minimum contract:

- create a move/split workflow;
- show/status with copy progress, catch-up frontier, current owner, target, and
  cutover readiness;
- validate data before cutover;
- dry-run cutover;
- switch traffic by committing the ownership transition;
- reverse or abort while source authority is still intact;
- complete and clean up artifacts after the new owner is stable.

### 5. Define The Cluster Operator Surface

RedDB should avoid scattered one-off endpoints. A coherent operator surface will
make the project feel more like infrastructure.

Suggested command groups:

```text
red cluster topology show
red cluster topology watch
red cluster health
red cluster validate
red cluster failover plan|execute|force
red cluster range move|split|status|cancel|complete
red cluster node drain|undrain
red cluster placement plan
```

The matching HTTP/gRPC/RedWire APIs should be stable enough for drivers, tests,
and future dashboards.

## Test Maturity Opportunities

The right next tests are workflow tests. RedDB already has useful lower-level
coverage, so the next gain comes from proving the product contract survives real
process failures.

Add these end-to-end scenarios:

| Scenario | Faults | Expected property |
|---|---|---|
| Stale router cache | Move ownership while a client keeps stale topology. | Client gets redirect or retry; stale owner rejects writes. |
| Planned failover | Freeze primary, catch target up, promote, demote old primary. | Zero skipped LSNs; reads and writes resume on new term. |
| Emergency failover | Kill primary before target is fully caught up. | Forced result surfaces skipped LSNs and rollback evidence. |
| Interrupted move range | Crash source/target/supervisor during copy or catch-up. | Catalog stays on old owner unless target covers watermark. |
| Cutover under traffic | Writes continue while target catches up. | Cutover only happens when target reaches watermark; no double writer. |
| Topology service outage | Topology refresh unavailable while cached routes exist. | Existing cached routes continue until stale; unsafe mutations fail closed. |
| Clock/timezone chaos | Change wall clock/timezone during leases and deadlines. | Monotonic deadlines hold; wall-clock metadata may change but safety does not. |
| Filesystem faults | Inject write/fsync/rename failures during control-plane state writes. | Vote/log/ownership state fails closed and remains recoverable. |

This ties directly to the broader chaos-OS direction: process death, network
partition, clock shifts, broken filesystem behavior, and corruption should be
attached to named cluster workflows.

## Recommended Issue Slices

1. **Cluster serving graph API**
   - Add a public topology snapshot endpoint and contract tests.
   - Include range/slot spans, owner, replicas, epoch, catalog version, and
     generation metadata.

2. **Router/gateway profile**
   - Expose a named runtime profile for topology-aware routing.
   - Add metrics for redirects, forwarding, cache age, and stale-epoch rejects.

3. **Planned failover command**
   - Wire the existing coordinated failover state machine to real transport.
   - Add a public command and an E2E test with one primary and two replicas.

4. **Emergency failover command**
   - Wire forced failover with explicit skipped-LSN and rollback evidence.
   - Require operator reason and durable event output.

5. **Move-range workflow status**
   - Wrap move-range state in a persisted workflow record.
   - Expose copy/catch-up/cutover readiness through CLI and API.

6. **Move-range cutover E2E**
   - Run real processes, writes during catch-up, crash/restart during move, and
     verify watermark-gated ownership transition.

7. **Cluster topology chaos pack**
   - Extend the black-box cluster harness with stale routes, topology refresh
     failure, primary isolation, target crash, filesystem write failures, and
     wall-clock/timezone shifts.

8. **Cluster operator documentation**
   - Document the operator workflow names before the runtime is complete.
   - Mark each as implemented, modeled, or roadmap to avoid overclaiming.

## Non-Goals

Do not put user writes through the control-plane consensus log. That would erase
one of RedDB's better architectural decisions and add consensus latency to the
normal write path.

Do not claim Vitess-level cluster maturity from ADRs alone. The current RedDB
state is best described as "strong cluster model with early runtime integration."

Do not implement distributed cross-shard write transactions as the first cluster
milestone. ADR 0055's rejection of hidden two-phase commit in the first cut is
still the right call.

## Acceptance Bar For Calling Cluster Production-Grade

Before RedDB should market clustered deployments as production-grade, the
following should be true:

- drivers or routers can fetch and refresh authoritative topology;
- stale topology produces redirects or safe failures, never split-brain writes;
- planned failover has a public zero-RPO workflow and E2E coverage;
- forced failover is explicit, auditable, and reports skipped data;
- range movement has status, validation, cutover, abort/reverse, and recovery;
- cluster bootstrap authority is enforced by the reserved global system range;
- process-level chaos tests cover failover, routing, and range movement;
- storage-fault tests cover control-plane vote/log/ownership persistence;
- docs distinguish implemented, modeled, and roadmap behavior.
