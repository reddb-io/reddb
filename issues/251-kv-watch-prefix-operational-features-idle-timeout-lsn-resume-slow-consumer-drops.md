# KV — WATCH prefix.* + operational features (idle timeout, LSN resume, slow-consumer drops) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/251

Labels: enhancement

GitHub issue number: #251

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Original GitHub Body

## Parent

#238

## What to build

Extends WATCH (#245) with the production-shape features. `WATCH prefix.*` subscribes to every change under a key prefix. Per-subscription buffers gain backpressure-aware drops with an exposed counter so slow consumers do not back-pressure writers. Disconnected subscribers can resume from a saved LSN to skip events they already saw. Idle subscriptions auto-expire after a configurable timeout to prevent orphan subscriptions accumulating after silent client disconnects.

Scope guard: this issue is **normal KV only**. Prefix watch for Config/Vault requires the metadata-safety rules in #321.

## Acceptance criteria

- [ ] Parser accepts `WATCH <prefix>.*`. Matching uses the same prefix index the engine already maintains for `SCAN <prefix>.*` queries.
- [ ] `KvWatchStream::subscribe_prefix(coll, prefix) -> Stream<Event>` reuses the CDC channel; events whose key matches the prefix are routed to the subscriber.
- [ ] Reconnect with `WATCH key FROM LSN <n>` resumes from the saved LSN. Events emitted on the missed window arrive in commit order before live events. Idempotent on duplicate LSN delivery.
- [ ] Per-subscription bounded buffer; when full, the oldest events drop and a `dropped_event_count` counter increments on the subscription. Stats endpoint surfaces the counter.
- [ ] Configurable per-environment idle timeout (`red.config.kv.watch.idle_timeout_ms`). After N ms with no events delivered AND no client read, the subscription is closed and resources reclaimed.
- [ ] Slow-consumer regression test: writer commits 10k events while a watcher reads at 100/s. Writer never blocks; watcher receives a tail with explicit drop counter.
- [ ] Reconnect test: subscribe → receive 5 events → disconnect → 50 more events commit → reconnect with the last seen LSN → receive the missed 50 in order, no duplicates.

## Blocked by

- #245
