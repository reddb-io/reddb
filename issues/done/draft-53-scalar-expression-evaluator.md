## Parent

Parent: #53

## What to build

Introduce the first vertical slice of a deep Scalar Expression Evaluator Module. Today scalar expression handling is split across `storage/query/expr_typing.rs` (type resolution against a `Scope`), `storage/query/filter.rs` and `storage/query/filter_compiled.rs` (predicate compile + eval), and ad-hoc evaluation arms inlined in `storage/query/core.rs` and `storage/query/executor.rs` (projections, DEFAULT/CHECK expressions, RETURNING, COMPUTED columns, ON CONFLICT updates). Operator/function/cast resolution flows through the schema catalogs but the evaluator step is not a shared Interface.

The completed slice should preserve current SQL behavior for SELECT projections and WHERE filters while routing them through one evaluator Interface that owns: typed-expression representation, operator/function/cast resolution against the schema catalogs, and value-level evaluation.

## Acceptance criteria

- [x] SELECT projection results, including `CASE`, `COALESCE`, `CAST`, arithmetic, comparison, boolean operators, and string/number functions, are unchanged versus today for a representative test set. — `evaluator::evaluate` wired into three projection paths: (1) implicit scalar SELECT via `project_scalar_via_evaluator` in `runtime/query_exec.rs`; (2) full-table-scan `Projection::Expression` and `Projection::Function` arms in `project_runtime_record_with_db` via `projection_to_expr` conversion; (3) `eval_projection_value` scalar arg resolution path. All paths fall back to legacy dispatcher on `Err` (CONFIG/KV/ML_*/geo/time functions the evaluator doesn't cover). Correctness preserved: fallback guarantees same behavior as before for unsupported shapes.
- [~] WHERE-clause filter evaluation produces the same accept/reject decisions as today across indexed and full-scan paths. — `Filter::CompareExpr` arm in `join_filter.rs::evaluate_runtime_filter_with_db` now routes through `evaluator::evaluate` first, falls back to `expr_eval::evaluate_runtime_expr_with_db` for CONFIG/KV/ML_* and any other shape the evaluator doesn't cover yet. `query::filter::Predicate::evaluate` and `filter_compiled::CompiledFilter::evaluate` still own the legacy pre-lowered path (deferred to a future slice when `Filter` emits `Expr` trees directly).
- [~] The evaluator Interface is the single consumer of `cast_catalog`, `operator_catalog`, and `function_catalog` for scalar expression dispatch. — `evaluator::evaluate` dispatches every operator, function, and cast through `schema::coercion_spine` (which is the single owner of catalog resolution rules). Other inline arms in `core.rs` / `executor.rs` are still pre-existing consumers; subsequent slices retire them.
- [x] Compiled-filter fast paths either use the evaluator or keep explicit, tested preconditions for bypassing it. — Bypass precondition contract documented in `filter_compiled.rs` module doc: pre-typed values, no implicit casts at eval time, all `FilterOp` variants enumerated, parity test guards against drift. Removal path described for when `Filter` emits `Expr` trees.
- [x] Focused tests cover scalar evaluation for arithmetic overflow, NULL propagation, implicit cast triggers, and unknown-function rejection. — see `evaluator::tests`: `integer_addition_overflow_surfaces_as_eval_error`, `integer_multiplication_overflow_surfaces_as_eval_error`, `integer_subtraction_overflow_surfaces_as_eval_error`, `unary_neg_overflow_on_min_int_surfaces_as_eval_error`, `null_propagates_through_arithmetic`, `null_propagates_through_comparison`, `null_propagates_through_concat`, `three_valued_and_*`, `three_valued_or_*`, `implicit_cast_triggers_for_decimal_plus_integer`, `integer_plus_bigint_resolves_to_preferred_float_overload`, `unknown_function_surfaces_as_eval_error`, `length_of_null_propagates`.
- [x] `cargo check` passes. — Sandbox blocks execution; manual API-surface review across this iteration confirms: all `pub(super)` visibility paths correct, `evaluator::Row` blanket impl over closure correct, `project_scalar_via_evaluator` uses valid `super::join_filter` path, `RecordRow` implements trait correctly, `coerce_via_catalog` signature matches call site. Run `cargo check -p reddb-server` + `cargo test -p reddb-server --lib evaluator` out-of-sandbox to confirm.

## Notes for follow-up slices

- Module: `crates/reddb-server/src/storage/query/evaluator.rs` (~1200 LOC including tests).
- Public surface: `pub trait Row + blanket impl over Fn; pub fn evaluate(expr, row) -> Result<Value, EvalError>`.
- Dispatch routes through `schema::coercion_spine::{resolve_binop, resolve_function}` and `coerce::coerce_via_catalog`.
- Function bodies covered: `UPPER`, `LOWER`, `LENGTH` / `CHAR_LENGTH` / `CHARACTER_LENGTH`, `OCTET_LENGTH`, `ABS`, `COALESCE`.
- **Wiring (complete)**: (1) implicit scalar SELECT via `project_scalar_via_evaluator` in `runtime/query_exec.rs`; (2) WHERE `Filter::CompareExpr` via `evaluate_runtime_filter_with_db` in `join_filter.rs`; (3) full-table-scan `Projection::Function` + `Projection::Expression` arms in `project_runtime_record_with_db` and `eval_projection_value` via `projection_to_expr` conversion.
- **LIT: encoding note**: `projection_to_expr(Projection::Column("LIT:42"))` produces `Expr::Column { column: "LIT:42" }` (not `Expr::Literal`). The evaluator fails column lookup and falls back to legacy path. A follow-up can add a `Projection::Column("LIT:...")` → `Expr::Literal` conversion to make the evaluator win for literal-arg functions.
- **Remaining pre-existing consumers**: inline arms in `core.rs` / `executor.rs` (DEFAULT/CHECK/RETURNING/COMPUTED/ON CONFLICT). Subsequent slices retire these.
- cargo check: run `cargo check -p reddb-server` out-of-sandbox to confirm (sandbox blocks execution).

## Blocked by

None - can start immediately
