# Investigate mutation↔event dual-write window in `WITH EVENTS`

Labels: correctness, investigation, needs-triage

## Problem

Tables declared `WITH EVENTS TO <queue>` emit events by calling `enqueue_event_payload` **after** the data mutation is persisted. See `crates/reddb-server/src/runtime/mutation.rs:548` and the call sites in `emit_insert_events_for_collection` / `emit_update_events_for_collection` / `emit_delete_events_for_collection`, plus the surrounding flow in `impl_dml.rs` where `persist_update_chunk` / `flush_update_chunk` run before `emit_update_events_for_collection`.

If the mutation's WAL append and the queue enqueue's WAL append are **separate WAL groups** with independent fsyncs, there is a crash window where:

- the data row mutation is durable, but the event for it is lost (downstream consumers never see it), or
- (less likely given current ordering) the event is durable but the mutation is not.

This is the classic dual-write problem the video describes, scoped to a single reddb process. The "transactional outbox" cure for an external broker doesn't apply here because the queue is another reddb Collection — the cure is making both writes part of one WAL group commit.

## What to investigate

1. Trace one UPDATE on a `WITH EVENTS` table through to WAL append. Are the data record and the queue record in:
   - (a) the same `TransactionFrame` / group commit (atomic — no window), or
   - (b) two separate WAL appends with independent fsync ordering (window exists)?

   Relevant files: `storage/wal/transaction.rs`, `storage/wal/group_commit.rs`, `storage/wal/append_coordinator.rs`, the `enqueue_event_payload` path.

2. Same investigation for INSERT and DELETE flows in `mutation.rs`.

3. Write a crash-injection test: kill the process between the mutation persist and the event enqueue, restart, verify the queue contains the event for every committed mutation. If the test is flaky or fails, the window is real.

## Decision after investigation

- If both writes share one WAL group commit: document this guarantee in `CONTEXT.md` under "Events & Subscriptions" and close.
- If there is a window: open a follow-up to fold the event payload into the same WAL group as the mutation (true internal outbox).

## Acceptance criteria

- [ ] Written analysis describing whether the window exists, with code citations.
- [ ] Reproduction test attempted (passes if no window, fails deterministically if window exists).
- [ ] If window exists: a follow-up issue filed with the proposed fix; this issue references it.
- [ ] If no window: `CONTEXT.md` updated with the guarantee.

## Out of scope

- Implementing the fix (separate issue if needed).
- External-broker outbox pattern (not relevant — reddb's queues are internal Collections).

## Blocked by

None - investigation can start immediately. Independent of #480.
