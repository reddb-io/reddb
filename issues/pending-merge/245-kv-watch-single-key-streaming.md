# KV — WATCH single-key streaming [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/245

Labels: enhancement

GitHub issue number: #245

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#238

## What to build

Adds push-stream subscriptions on a single KV key. `WATCH key` opens a server-streaming channel; every committed PUT / DELETE / INCR / CAS on that key emits an event with the previous value, the new value, the operation, and the LSN. Replaces polling for feature-flag fan-out, live config, presence-style patterns. Ships across gRPC server-streaming and HTTP Server-Sent Events.

## Acceptance criteria

- [ ] Parser accepts `WATCH <key>`. The statement is server-streaming, not a regular query — the response shape is a sequence of events.
- [ ] `KvWatchStream` runtime module exists with `subscribe(coll, key) -> Stream<Event>`. Each `Event` carries `{ key, op, before, after, lsn, committed_at }`.
- [ ] Stream rides the engine's existing CDC channel (the one result-cache invalidation already uses). No parallel notification fabric.
- [ ] Only committed events are emitted — never dirty-read or rolled-back transitions. Verified by an integration test that opens a transaction, mutates the key, rolls back, and asserts the watcher received nothing.
- [ ] gRPC method `KvWatch` returns a server stream of events with the wire-level `Event` shape.
- [ ] HTTP `GET /collections/<coll>/kv/<key>/watch` returns SSE (`text/event-stream`) with the same payload.
- [ ] Drivers (Node, Python, Rust) expose `db.kv.watch(key) -> AsyncIterable<Event>` (or the language-equivalent).
- [ ] Delivery-order test: PUT → INCR → DELETE → PUT (rapid sequence) on a watched key must deliver the four events in commit order, with monotonically increasing LSNs.
- [ ] No regression on the existing CDC channel users (result cache, table triggers).

## Blocked by

- #241
