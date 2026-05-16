# ADR 0015: `WITH EVENTS` Dual-Write Window

## Status

Resolved by <https://github.com/reddb-io/reddb/issues/448>.

## Context

Before issue #448, `WITH EVENTS` emitted mutation events by writing the data
mutation first and then enqueueing the event payload into the target queue.

The relevant write path is:

- `crates/reddb-server/src/runtime/mutation.rs`: row inserts call
  `insert_auto`, emit CDC, then call `emit_insert_events_for_collection`.
- `crates/reddb-server/src/runtime/impl_dml.rs`: updates call
  `persist_update_chunk`, then `flush_update_chunk`, then
  `emit_update_events_for_collection`; deletes call `delete_entities_batch`,
  then `emit_delete_events_for_collection`.
- `crates/reddb-server/src/runtime/mutation.rs`: `emit_*_events_for_collection`
  calls `enqueue_event_payload`.
- `crates/reddb-server/src/runtime/impl_queue.rs`: `enqueue_event_payload_raw`
  inserts a queue message with `store.insert_auto`.
- `crates/reddb-server/src/storage/unified/store/commit.rs`: each
  `finish_paged_write` outside explicit transaction capture calls
  `StoreCommitCoordinator::append_actions`, producing its own
  `WalRecord::TxCommitBatch` and durability wait.

Therefore, in the old autocommit path, the source-row write and the
queue-message write were separate store WAL commit batches. The queue write
could share neither the same `TxCommitBatch` nor the same caller-level atomic
commit decision as the source mutation.

## Decision

Autocommit DML statements that may emit `WITH EVENTS` payloads now defer store
WAL actions until the statement finishes, then append the source mutation and
event queue write in one `TxCommitBatch`. That makes recovery observe both the
source mutation and its event, or neither, at the store WAL boundary.

The fixed design closes this historical crash window:

1. The source mutation may become durable.
2. The process may crash before the event queue message is appended and made
   durable.
3. On restart, the row exists but downstream consumers never see the event.

The opposite ordering is less likely in the current flow because the queue write
happens after the mutation write, but the implementation should not rely on two
independent WAL appends for a single logical mutation event.

## Evidence

`runtime::impl_dml::tests::with_events_autocommit_persists_mutation_and_event_in_one_wal_batch`
is a WAL invariant test. It creates a persistent `WITH EVENTS` table, executes
one autocommit `INSERT`, reads the store WAL, and asserts that the source table
write and the event queue write appear in the same `WalRecord::TxCommitBatch`.

This is a deterministic proxy for the crash-injection scenario: there is no
store-WAL position where the source row action is durable but the event queue
action is absent.

## Consequences

- Documentation may describe autocommit `WITH EVENTS` source/event persistence
  as store-WAL atomic.
- Consumers should continue deduplicating by `event_id`, but deduplication does
  not repair a missing event.
- Backpressure and DLQ routing still happen before the statement's store WAL
  batch is appended, so a successfully routed event or DLQ record is persisted
  with the source mutation.
