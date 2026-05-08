# KV — TAGS clause on PUT + tag-based invalidation [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/246

Labels: enhancement

GitHub issue number: #246

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#238

## What to build

Brings the Cache primitive's tag invalidation grammar to user KV collections. `PUT key = value TAGS [...]` attaches one or more tags at write time; `INVALIDATE TAGS [...] FROM <coll>` drops every entry tagged with any of the listed values in O(matching) time. Symmetry across primitives — operators learn the model once and apply it everywhere.

## Acceptance criteria

- [ ] Parser accepts `PUT <key> = <value> [EXPIRE …] [TAGS [t1, t2, …]]`.
- [ ] Parser accepts `INVALIDATE TAGS [t1, t2, …] FROM <kv-collection>`.
- [ ] Tag index is maintained incrementally on PUT / DELETE — no full scan required for invalidation. Same shape as the Cache primitive's tag index.
- [ ] `INVALIDATE TAGS […]` removes every entry tagged with at least one of the listed tags. Returns the count of entries removed.
- [ ] All transports + all drivers expose the new shape.
- [ ] Integration test: tag a session blob with `[user:42, org:7]`. Invalidating by `[user:42]` removes it; invalidating by `[org:99]` does not.
- [ ] Tag invalidation respects policy actions — a principal without `kv:invalidate` cannot run the verb even on collections they can `PUT` into.

## Blocked by

- #241
