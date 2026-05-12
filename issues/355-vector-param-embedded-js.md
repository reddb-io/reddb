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

## Progress (2026-05-12)

Partial vertical slice landed: SEARCH SIMILAR `$N` end-to-end, JSON-RPC vector
mapping, and JS Float32Array serialization. INSERT VALUES with vector params
deferred — see remaining work below.

Done:

- `Value::Vector` already exists in the engine `Value` enum (no schema work
  needed beyond wiring).
- `rpc_stdio::json_value_to_schema_value` now maps a JSON array of numbers
  (incl. empty) to `Value::Vector(Vec<f32>)`. Mixed/non-numeric arrays still
  fall back to JSON-string form so the binder can surface a typed error.
- Parser: `SEARCH SIMILAR $N COLLECTION ...` accepts a positional placeholder
  in the vector slot. Honours the existing `?`/`$N` mixing guard.
- AST: `SearchCommand::Similar` gained `vector_param: Option<usize>`. All
  construction/match sites updated (parser, vector_search_snapshots,
  parser/tests, impl_graph_commands runtime guard).
- Binder: `storage::query::user_params::bind` handles
  `QueryExpr::SearchCommand(Similar)` directly — pulls the bound
  `Value::Vector` into the `vector` field and clears `vector_param`. Rejects
  non-vector values with new `UserParamError::TypeMismatch { slot, got }`,
  which the stdio layer maps to `INVALID_PARAMS`.
- JS SDK: `query(sql, params)` runs each param through a new
  `serializeParam()` that converts `Float32Array` / `Float64Array` to a plain
  number array on the wire. Plain `number[]` round-trips as-is.
- Tests:
  - `user_params::tests::bind_search_similar_vector_param` (happy path)
  - `bind_search_similar_rejects_non_vector_param` (typed error)
  - `bind_search_similar_empty_vector_param`
  - `drivers/js/test/smoke.test.mjs` — new "parameterized SEARCH SIMILAR $N
    with vector param (#355)" case covering `number[]`, `Float32Array`, and
    the type-mismatch rejection.

Deferred to follow-up — keep this issue open until done:

- `INSERT INTO <coll> (dense, ...) VALUES ($1, ...)`. The parser folds
  `value_exprs` to `values` via `fold_expr_to_value`, which currently errors
  on `Expr::Parameter`. Wiring requires either (a) folding `Parameter` to a
  sentinel `Value` plus a re-fold step inside `user_params::bind`, or
  (b) deferring fold until after binding. Both are bigger than the SEARCH
  SIMILAR slot — split out so this issue can ship the search-side win.
- Large-vector dimensional sweep (1024 / 4096) — straightforward extension
  of the JS smoke test once INSERT is wired.

Files touched:

- `crates/reddb-server/src/rpc_stdio.rs`
- `crates/reddb-server/src/storage/query/core.rs`
- `crates/reddb-server/src/storage/query/parser/search_commands.rs`
- `crates/reddb-server/src/storage/query/parser/tests.rs`
- `crates/reddb-server/src/storage/query/user_params.rs`
- `crates/reddb-server/src/runtime/impl_graph_commands.rs`
- `crates/reddb-server/tests/vector_search_snapshots.rs`
- `drivers/js/src/index.js`
- `drivers/js/test/smoke.test.mjs`
