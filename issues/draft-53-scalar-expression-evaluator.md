## Parent

Parent: #53

## What to build

Introduce the first vertical slice of a deep Scalar Expression Evaluator Module. Today scalar expression handling is split across `storage/query/expr_typing.rs` (type resolution against a `Scope`), `storage/query/filter.rs` and `storage/query/filter_compiled.rs` (predicate compile + eval), and ad-hoc evaluation arms inlined in `storage/query/core.rs` and `storage/query/executor.rs` (projections, DEFAULT/CHECK expressions, RETURNING, COMPUTED columns, ON CONFLICT updates). Operator/function/cast resolution flows through the schema catalogs but the evaluator step is not a shared Interface.

The completed slice should preserve current SQL behavior for SELECT projections and WHERE filters while routing them through one evaluator Interface that owns: typed-expression representation, operator/function/cast resolution against the schema catalogs, and value-level evaluation.

## Acceptance criteria

- [ ] SELECT projection results, including `CASE`, `COALESCE`, `CAST`, arithmetic, comparison, boolean operators, and string/number functions, are unchanged versus today for a representative test set.
- [ ] WHERE-clause filter evaluation produces the same accept/reject decisions as today across indexed and full-scan paths.
- [ ] The evaluator Interface is the single consumer of `cast_catalog`, `operator_catalog`, and `function_catalog` for scalar expression dispatch.
- [ ] Compiled-filter fast paths either use the evaluator or keep explicit, tested preconditions for bypassing it.
- [ ] Focused tests cover scalar evaluation for arithmetic overflow, NULL propagation, implicit cast triggers, and unknown-function rejection.
- [ ] `cargo check` passes.

## Blocked by

None - can start immediately
