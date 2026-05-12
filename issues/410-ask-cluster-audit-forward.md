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
