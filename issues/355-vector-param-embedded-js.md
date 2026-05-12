# Vector parameter support end-to-end via embedded stdio + JS driver [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/355

Labels: needs-triage

GitHub issue number: #355

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Vector parameter support end-to-end. A TypeScript caller can run:

```typescript
const vec = await embed('user query')  // number[] or Float32Array
const hits = await db.query(
  'SEARCH SIMILAR $1 IN embeddings K 5 MIN_SCORE 0.7',
  [vec],
)
```

Adds `Value::Vector(Vec<f32>)` to the engine Value enum, vector context to the binder (the SEARCH SIMILAR vector slot accepts `Value::Vector` and rejects others with a typed error), and JS SDK serialization for `number[]` and `Float32Array`.

INSERT with vector parameters also works:

```typescript
await db.query(
  'INSERT INTO embeddings (dense, content) VALUES ($1, $2)',
  [vec, 'doc text'],
)
```

K, MIN_SCORE, and other clauses are out of scope (see #357 — clause bind expansion).

## Acceptance criteria

- [ ] `Value::Vector` round-trips through embedded stdio JSON-RPC.
- [ ] `SEARCH SIMILAR $1 IN <coll> K <int>` accepts vector param and returns results.
- [ ] `INSERT INTO <coll> (dense, ...) VALUES ($1, ...)` accepts vector param.
- [ ] Binder rejects non-vector value in vector context with a typed error.
- [ ] JS SDK accepts `number[]` and `Float32Array` and serializes correctly.
- [ ] Empty vector and large vectors (1024-dim, 4096-dim) work.
- [ ] Integration test in `drivers/js/test/` covering insert + search.

## Blocked by

- #353
