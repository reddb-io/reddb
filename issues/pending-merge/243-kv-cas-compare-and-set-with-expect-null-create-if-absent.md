# KV — CAS (compare-and-set) with EXPECT NULL create-if-absent [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/243

Labels: enhancement

GitHub issue number: #243

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#238

## What to build

Adds compare-and-set as an engine-native primitive. `CAS key EXPECT old SET new EXPIRE <duration>` succeeds only when the current value is structurally equal to `old`, otherwise returns the actual current value so the caller can retry or branch. `CAS key EXPECT NULL SET v` is the create-if-absent shape — useful for distributed locks, leader election, and idempotent state transitions in one round trip.

## Acceptance criteria

- [ ] Parser accepts `CAS <key> EXPECT <expected> SET <new> [EXPIRE <duration>]`. `<expected>` may be a literal value or `NULL`; `<new>` is any literal.
- [ ] `KvAtomicOps::cas(coll, key, expected, new, ttl_ms?)` returns `{ ok: bool, current: Option<Value> }`. Caller branches on `ok`; uses `current` to retry without a re-read.
- [ ] Equality is typed: `EXPECT 'free'` checks the persisted typed `Value`, not its serialised text. `EXPECT NULL` matches a missing key (and only a missing key).
- [ ] `EXPIRE` clause sets the TTL on the new value when the CAS succeeds. No-op when the CAS fails.
- [ ] All transports + all drivers expose `cas` with the same envelope shape. Drivers return a typed result (`{ ok, current }`).
- [ ] Property-based race test: N concurrent CAS calls on the same key with the same `expected` succeed exactly once; the other N-1 receive the new `current` value with `ok: false` and no retry needed to read it.
- [ ] Lock pattern integration test: `CAS lock.deploy EXPECT 'free' SET 'worker-7' EXPIRE 30s` taken once, parallel callers fail. The holding worker releases via `CAS lock.deploy EXPECT 'worker-7' SET 'free'` and the next caller acquires.

## Blocked by

- #242
