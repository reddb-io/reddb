# Answer cache opt-in CACHE TTL + settings defaults + NOCACHE [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/403

Labels: needs-triage

GitHub issue number: #403

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Opt-in answer cache: `ASK '...' CACHE TTL '5m'` populates an entry keyed by `hash(question + provider + model + temperature + seed + sources_fingerprint)`.

Settings: `ask.cache.enabled` (default false), `ask.cache.default_ttl`, `ask.cache.max_entries`. Per-query `NOCACHE` bypasses the default.

Reuses the Blob Cache infrastructure (ADR 0006) for storage and invalidation. Mutations to source collections invalidate via the existing dependency-invalidation mechanism.

## Acceptance criteria

- [ ] `ASK '...' CACHE TTL '5m'` populates cache; subsequent identical call returns `cache_hit: true` and skips LLM.
- [ ] Audit row still written on cache hit (with `cache_hit: true`).
- [ ] `ask.cache.enabled` setting enables global default; `NOCACHE` bypasses.
- [ ] Mutation to a source collection invalidates affected cache entries.
- [ ] Integration test: insert + ASK CACHE + insert again + ASK same question → second call sees fresh data.
- [ ] Cache key respects per-user/tenant scope (no cross-tenant leak).

## Blocked by

- #402
