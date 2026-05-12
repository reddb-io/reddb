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
- `K $N` in SEARCH HYBRID / spatial / context — same shape as LIMIT.
- `SEARCH SIMILAR TEXT $N` — text param into the embedding pipeline;
  the embedding provider call is the only wrinkle. Same parser
  routing as the existing TEXT 'literal' branch.
- `PROBES $N` (IVF) — slot lives outside SearchCommand::Similar.

`?` placeholder works at the helper level but parse_multi routes any
`?`-bearing input to the SPARQL frontend, so an end-to-end `?` test
for SEARCH SIMILAR LIMIT is deferred alongside detector tightening.
