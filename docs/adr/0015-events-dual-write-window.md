# ADR 0015: `WITH EVENTS` Dual-Write Window

## Status

Accepted as current-risk documentation. Follow-up fix required:
<https://github.com/reddb-io/reddb/issues/448>.

## Context

`WITH EVENTS` currently emits mutation events by writing the data mutation first
and then enqueueing the event payload into the target queue.

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

Therefore, in autocommit mode, the source-row write and the queue-message write
are separate store WAL commit batches. The queue write can share neither the
same `TxCommitBatch` nor the same caller-level atomic commit decision as the
source mutation.

## Decision

A dual-write crash window exists for autocommit `WITH EVENTS` mutations:

1. The source mutation may become durable.
2. The process may crash before the event queue message is appended and made
   durable.
3. On restart, the row exists but downstream consumers never see the event.

The opposite ordering is less likely in the current flow because the queue write
happens after the mutation write, but the implementation should not rely on two
independent WAL appends for a single logical mutation event.

## Evidence

`runtime::impl_dml::tests::with_events_autocommit_currently_splits_mutation_and_event_wal_batches`
is a WAL characterization test. It creates a persistent `WITH EVENTS` table,
executes one autocommit `INSERT`, reads the store WAL, and asserts that the
source table write and the event queue write appear in different
`WalRecord::TxCommitBatch` records.

This is a deterministic proxy for the crash-injection scenario: if a crash lands
after the first batch is durable and before the second batch is durable, recovery
can observe the source mutation without the corresponding queue event.

## Consequences

- Documentation must not claim that current `WITH EVENTS` delivery is an
  already-atomic internal outbox.
- Consumers should continue deduplicating by `event_id`, but deduplication does
  not repair a missing event.
- The fix should fold event queue payloads into the same store WAL batch as the
  source mutation, or introduce a real internal outbox record written in the
  same batch and drained afterward.
