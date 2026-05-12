# Bind support in K / LIMIT / OFFSET / MIN_SCORE / SEARCH SIMILAR TEXT clauses [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/361

Labels: needs-triage

GitHub issue number: #361

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Extend the binder to accept parameters in additional clauses beyond VALUES / WHERE:

- `K $N` (vector top-k)
- `LIMIT $N` / `OFFSET $N`
- `MIN_SCORE $N`
- `SEARCH SIMILAR TEXT $N USING <provider>` (text param for auto-embedded search)
- `PROBES $N` (IVF)
- Any other clause that today accepts a literal but should accept a parameter

Each clause requires a typed binder context (integer for K/LIMIT/OFFSET/PROBES, float for MIN_SCORE, text for SIMILAR TEXT). Mismatches return typed errors.

## Acceptance criteria

- [ ] All listed clauses accept `$N` parameters and reject wrong types with typed errors.
- [ ] `db.query('SEARCH SIMILAR $1 IN embeddings K $2 MIN_SCORE $3', [vec, 5, 0.7])` works end-to-end.
- [ ] `db.query('SEARCH SIMILAR TEXT $1 COLLECTION docs USING openai', [text])` works.
- [ ] Tests cover each clause with both correct and incorrect parameter types.

## Blocked by

- #355

## Progress (2026-05-12)

Tracer slice landed: `SEARCH SIMILAR ... LIMIT $N MIN_SCORE $N`
parameterization end-to-end through the same binder used by the existing
vector-slot path (#355).

Done in this slice:
- `SearchCommand::Similar` gained `limit_param: Option<usize>` and
  `min_score_param: Option<usize>` (AST in `storage/query/core.rs`).
- Parser routes `$N` (and `?`, mode permitting) in the `LIMIT` and
  `MIN_SCORE` slots via a new shared helper `Parser::parse_param_slot`
  in `parser/mod.rs`. Lives next to `parse_integer` so future SELECT
  / OFFSET / K / PROBES slots can reuse it without duplication.
- `user_params::bind` binds the new slots with typed errors:
  - LIMIT: accepts Integer / UnsignedInteger / BigInt with N > 0;
    rejects 0/negative with `LIMIT parameter (must be > 0)`.
  - MIN_SCORE: accepts Float and any integer family (widened to f32).
- `runtime/impl_graph_commands.rs` asserts both new params are bound
  pre-execution (same defense-in-depth pattern as vector_param).
- Tests in `user_params` (6 new): limit_param happy path, min_score
  happy path, both together with vector_param, LIMIT rejects
  non-integer, LIMIT rejects 0/negative, MIN_SCORE rejects non-numeric.
- TypeMismatch Display generalized from "requires a vector" to
  "(got {variant})" so the same enum can describe non-vector slots.

Deferred to follow-up slices:
- SELECT `LIMIT $N` / `OFFSET $N` — needs param slots on TableQuery
  (multiple AST sites; lift cleanly once SELECT shape binder grows
  non-Expr param slot support).
- `SEARCH SIMILAR TEXT $N` — text param into the embedding pipeline;
  the embedding provider call is the only wrinkle. Same parser
  routing as the existing TEXT 'literal' branch.
- `PROBES $N` (IVF) — slot lives outside SearchCommand::Similar.
- `K $N` in SEARCH CONTEXT / SPATIAL — same shape as HYBRID; trivial.

## Progress (2026-05-12, slice 2)

Second tracer slice landed: `SEARCH HYBRID ... LIMIT $N` (and `K $N`,
which the HYBRID parser treats as a LIMIT alias).

Done in this slice:
- `SearchCommand::Hybrid` gained `limit_param: Option<usize>` mirroring
  the `Similar::limit_param` shape from slice 1.
- Parser routes `$N` (and `?`, mode permitting) in HYBRID LIMIT / K via
  the same `parse_param_slot` helper.
- `user_params::collect_non_expr_indices` now walks Hybrid as well.
- `user_params::bind` gained a Hybrid branch with the same typed error
  set as the SIMILAR LIMIT path: non-integer rejected, 0/negative
  rejected with `(must be > 0)`.
- `runtime/impl_graph_commands.rs` guards the new param.
- Existing destructures in `parser/tests.rs` and
  `tests/vector_search_snapshots.rs` switched to `..` so future slots
  can be added without churning unrelated tests.
- Tests in `user_params` (4 new): hybrid LIMIT happy path, K alias
  happy path, LIMIT rejects non-integer, LIMIT rejects 0.

`?` placeholder works at the helper level but parse_multi routes any
`?`-bearing input to the SPARQL frontend, so an end-to-end `?` test
for SEARCH SIMILAR LIMIT is deferred alongside detector tightening.

## Progress (2026-05-12, slice 3)

Third slice landed: `SEARCH SPATIAL NEAREST ... K $N`.

- `SearchCommand::SpatialNearest` gained `k_param: Option<usize>`
  (AST in `storage/query/core.rs`), same shape as `Hybrid::limit_param`.
- Parser routes `$N` (and `?`, mode permitting) in the NEAREST K slot
  via `parse_param_slot`. Literal path keeps `parse_positive_integer`.
- `user_params::collect_non_expr_indices` matches SpatialNearest.
- `user_params::bind` gained a SpatialNearest branch with the same
  typed error set as the SIMILAR / HYBRID LIMIT paths.
- `runtime/impl_graph_commands.rs` guards the new param.
- `tests/geo_parser.rs` destructure switched to `..` for future slots.
- Tests in `user_params` (3 new): NEAREST K happy path, NEAREST K
  rejects 0, NEAREST K rejects non-integer.

Remaining slices documented above (SELECT LIMIT / OFFSET, SIMILAR
TEXT $N, PROBES, K in SEARCH CONTEXT — note CONTEXT actually uses
LIMIT/DEPTH not K so the prior progress note was imprecise; LIMIT
parameterization for SEARCH TEXT / MULTIMODAL / INDEX / CONTEXT and
the SPATIAL RADIUS/BBOX `LIMIT` slot are trivial follow-ups now that
the SpatialNearest pattern is in place).

## Progress (2026-05-12, slice 4)

Fourth slice landed: `SEARCH TEXT ... LIMIT $N`.

- `SearchCommand::Text` gained `limit_param: Option<usize>` (AST in
  `storage/query/core.rs`), same shape as `Hybrid::limit_param`.
- Parser routes `$N` (and `?`, mode permitting) in the SEARCH TEXT
  LIMIT slot via `parse_param_slot`.
- `user_params::collect_non_expr_indices` matches Text.
- `user_params::bind` gained a Text branch with the same typed error
  set as the SIMILAR / HYBRID / SPATIAL NEAREST LIMIT paths.
- `runtime/impl_graph_commands.rs` guards the new param.
- `parser/tests.rs` destructures switched to `..` for future slots.
- Tests in `user_params` (3 new): TEXT LIMIT happy path, rejects 0,
  rejects non-integer.

Remaining LIMIT $N slices (same trivial pattern, one variant each):
MULTIMODAL, INDEX, CONTEXT, SPATIAL RADIUS, SPATIAL BBOX. Plus
SELECT LIMIT / OFFSET (TableQuery shape), SIMILAR TEXT $N (text
embedding pipeline), and PROBES $N (IVF).

## Progress (2026-05-12, slice 5)

Fifth slice landed: `SEARCH MULTIMODAL ... LIMIT $N`.

- `SearchCommand::Multimodal` gained `limit_param: Option<usize>` (AST
  in `storage/query/core.rs`), same shape as `Hybrid::limit_param`.
- Parser routes `$N` (and `?`, mode permitting) in MULTIMODAL LIMIT
  via `parse_param_slot`.
- `user_params::collect_non_expr_indices` matches Multimodal.
- `user_params::bind` gained a Multimodal branch with the same typed
  error set as the SIMILAR / HYBRID / SPATIAL NEAREST / TEXT LIMIT
  paths.
- `runtime/impl_graph_commands.rs` guards the new param.
- `parser/tests.rs` destructures switched to `..` for future slots.
- Tests in `user_params` (3 new): MULTIMODAL LIMIT happy path,
  rejects 0, rejects non-integer.

Remaining LIMIT $N slices: INDEX, CONTEXT, SPATIAL RADIUS, SPATIAL
BBOX. Plus SELECT LIMIT / OFFSET (TableQuery shape), SIMILAR TEXT $N
(text embedding pipeline), and PROBES $N (IVF).

## Progress (2026-05-12, slice 6)

Sixth slice landed: `SEARCH INDEX ... LIMIT $N`.

- `SearchCommand::Index` gained `limit_param: Option<usize>` (AST in
  `storage/query/core.rs`), same shape as `Hybrid::limit_param`.
- Parser routes `$N` (and `?`, mode permitting) in INDEX LIMIT via
  `parse_param_slot`.
- `user_params::collect_non_expr_indices` matches Index.
- `user_params::bind` gained an Index branch with the same typed
  error set as the SIMILAR / HYBRID / SPATIAL NEAREST / TEXT /
  MULTIMODAL LIMIT paths.
- `runtime/impl_graph_commands.rs` guards the new param.
- `parser/tests.rs` destructures (2) switched to `..` for future
  slots.
- Tests in `user_params` (3 new): INDEX LIMIT happy path, rejects 0,
  rejects non-integer.

Remaining LIMIT $N slices: CONTEXT, SPATIAL RADIUS, SPATIAL BBOX.
Plus SELECT LIMIT / OFFSET (TableQuery shape), SIMILAR TEXT $N
(text embedding pipeline), and PROBES $N (IVF).

## Progress (2026-05-12, slice 7)

Seventh slice landed: `SEARCH CONTEXT ... LIMIT $N`.

- `SearchCommand::Context` gained `limit_param: Option<usize>` (AST in
  `storage/query/core.rs`), same shape as `Hybrid::limit_param`.
- Parser routes `$N` (and `?`, mode permitting) in CONTEXT LIMIT via
  `parse_param_slot`. The 2-iteration LIMIT/DEPTH loop now branches
  on Token::Dollar/Question for the LIMIT slot only; DEPTH stays
  literal.
- `user_params::collect_non_expr_indices` matches Context.
- `user_params::bind` gained a Context branch with the same typed
  error set as the SIMILAR / HYBRID / SPATIAL NEAREST / TEXT /
  MULTIMODAL / INDEX LIMIT paths.
- `runtime/impl_graph_commands.rs` guards the new param.
- `parser/tests.rs` (2) and `tests/ask_parser.rs` (2) destructures
  switched to `..` for future slots.
- Tests in `user_params` (3 new): CONTEXT LIMIT happy path,
  rejects 0, rejects non-integer.

Remaining LIMIT $N slices: SPATIAL RADIUS, SPATIAL BBOX.
Plus SELECT LIMIT / OFFSET (TableQuery shape), SIMILAR TEXT $N
(text embedding pipeline), and PROBES $N (IVF).

## Progress (2026-05-12, slice 8)

Eighth slice landed: `SEARCH SPATIAL RADIUS ... LIMIT $N`.

- `SearchCommand::SpatialRadius` gained `limit_param: Option<usize>`
  (AST in `storage/query/core.rs`), same shape as
  `Hybrid::limit_param`.
- Parser routes `$N` (and `?`, mode permitting) in SPATIAL RADIUS
  LIMIT via `parse_param_slot`. Literal path keeps `parse_integer()`.
- `user_params::collect_non_expr_indices` matches SpatialRadius.
- `user_params::bind` gained a SpatialRadius branch with the same
  typed error set as the SIMILAR / HYBRID / SPATIAL NEAREST /
  TEXT / MULTIMODAL / INDEX / CONTEXT LIMIT paths.
- `runtime/impl_graph_commands.rs` guards the new param.
- `tests/geo_parser.rs` SpatialRadius destructure switched to `..`
  for future slots.
- Tests in `user_params` (3 new): RADIUS LIMIT happy path,
  rejects 0, rejects non-integer.

Remaining LIMIT $N slices: SPATIAL BBOX.
Plus SELECT LIMIT / OFFSET (TableQuery shape), SIMILAR TEXT $N
(text embedding pipeline), and PROBES $N (IVF).
