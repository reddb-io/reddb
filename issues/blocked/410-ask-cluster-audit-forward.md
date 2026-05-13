# Cluster: ASK on replica + audit forward primary-sync [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/410

Labels: enhancement

GitHub issue number: #410

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

ASK on read replicas: retrieval reads local snapshot; LLM call from the local node; audit + cost forwarded synchronously to primary before answer returns.

If primary unreachable, replica returns 503 — no audit gap permitted. Cache populate is local + async-propagate.

Reuses the existing replication module's primary-sync RPC mechanism.

## Acceptance criteria

- [ ] ASK accepts on read replicas.
- [ ] Retrieval served from local snapshot (no primary roundtrip for read path).
- [ ] Audit row write forwarded to primary synchronously; answer waits for ACK.
- [ ] Cost-counter increment forwarded synchronously to primary.
- [ ] Primary unreachable → ASK on replica returns 503; no audit gap.
- [ ] Cache populate is async-local + propagate.
- [ ] Integration test in a 1-primary + 2-replica cluster harness.

## Blocked by

- #402

## Progress note (2026-05-13)

Rechecked after #402 and #403 closed. The ASK audit writer and answer
cache now exist, so the original listed blocker is no longer the
limiting factor.

The remaining blocker is architectural: current replication gRPC exposes
WAL pull/ack (`PullWalRecords`, `AckReplicaLsn`) but no primary-sync RPC
for a replica to durably submit non-WAL side effects such as `red_ask_audit`
rows and daily cost counter increments before returning an ASK answer.
`execute_ask` is synchronous and is also called from async gRPC handlers,
so adding this safely requires a defined primary-sync command/RPC contract
and async/sync boundary rather than a one-off transport call hidden inside
the ASK path.

Leaving runtime code unchanged and keeping this issue blocked until the
primary-sync RPC contract/harness exists. Acceptance items requiring a
1-primary + 2-replica harness remain unverified.

## Progress note (2026-05-13, primary-sync slice)

Added the first primary-sync contract slice:

- `SubmitAskSideEffects` gRPC endpoint on primaries.
- Replica ASK cost accounting forwards `ask.side_effects.v1` usage to the
  primary synchronously.
- Replica ASK audit rows forward to the primary synchronously before the
  answer returns.
- Primary-sync unavailability maps to HTTP 503 / gRPC unavailable.
- gRPC `Ask` runs the synchronous runtime path from a blocking task so the
  forwarding call does not block the async reactor.

Verified with:

- `cargo check --locked -p reddb-io-server`
- `cargo test --locked -p reddb-io-server primary_ask_side_effects`
- `cargo test --locked -p reddb-io-server ask_audit_retention_purge_deletes_rows_older_than_setting`

Still not done:

- Full 1-primary + 2-replica integration harness coverage.
- Cache async-propagate behavior is still unverified/not implemented here.
- The issue should remain open until the remaining acceptance criteria are
  covered.
