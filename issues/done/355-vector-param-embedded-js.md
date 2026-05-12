# Vector parameter support end-to-end via embedded stdio + JS driver [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/355

Labels: enhancement

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

## Progress (2026-05-12) — INSERT half landed

- Parser `parse_insert_query`: when an expression in VALUES contains
  `Expr::Parameter`, fold to a `Value::Null` placeholder rather than
  erroring out. Non-parameter folding errors still surface.
- `user_params::expr_contains_parameter` + `substitute_params_in_expr`:
  new helpers that detect placeholders and rewrite an `Expr` tree by
  swapping each `Expr::Parameter` for an `Expr::Literal` carrying the
  caller-supplied value.
- `user_params::collect_indices` now traverses `QueryExpr::Insert`
  rows so arity/gap validation covers INSERT placeholders.
- `user_params::bind` handles `QueryExpr::Insert`: substitutes
  parameters in `value_exprs`, re-folds each row to refresh `values`,
  and returns the bound `InsertQuery`. Type validation (vector slot,
  etc.) remains the engine type checker's job downstream.
- Tests: `bind_insert_values_with_vector_param`,
  `bind_insert_arity_mismatch` (`user_params::tests` now 13 passed).
- JS smoke test: new
  "parameterized INSERT VALUES with vector param (#355)" case inserts
  a vector via `$1`/`$2`, then SEARCH SIMILAR finds the inserted row.

Large-vector dimensional sweep (1024 / 4096) still TODO — minor
extension of the smoke test, deferred to a wire-codec / driver issue.

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
