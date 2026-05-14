# Fix WITH EVENTS dual-write window [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/448

## Problem

Autocommit `WITH EVENTS` currently writes the source mutation and event queue message as separate store WAL `TxCommitBatch` records.

Crash window:

1. source row mutation is durable;
2. process crashes before the event queue write is durable;
3. after restart, downstream consumers never see the event for the committed mutation.

See `docs/adr/0015-events-dual-write-window.md` and the fixed invariant tests:

`runtime::impl_dml::tests::with_events_autocommit_persists_mutation_and_event_in_one_wal_batch`
`runtime::impl_dml::tests::with_events_autocommit_update_persists_mutation_and_event_in_one_wal_batch`
`runtime::impl_dml::tests::with_events_autocommit_delete_persists_mutation_and_event_in_one_wal_batch`

## Relevant Paths

- `crates/reddb-server/src/runtime/mutation.rs`
- `crates/reddb-server/src/runtime/impl_queue.rs`
- `crates/reddb-server/src/storage/unified/store/commit.rs`
- `docs/data-models/events.md`
- `docs/adr/0015-events-dual-write-window.md`

## Acceptance Criteria

- [x] Source mutation and event/outbox record are persisted in one atomic store WAL batch.
- [x] Crash or WAL-level test covers the mutation-durable/event-missing boundary.
- [x] Current characterization test is updated to assert the fixed invariant.
- [x] Docs remove the current-risk caveat.
- [x] Commit the completed work.
- [x] Move this issue file to `issues/done/` when complete.
