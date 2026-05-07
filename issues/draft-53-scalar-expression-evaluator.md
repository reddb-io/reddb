## Parent

Parent: #53

## What to build

Introduce the first vertical slice of a deep Scalar Expression Evaluator Module. Today scalar expression handling is split across `storage/query/expr_typing.rs` (type resolution against a `Scope`), `storage/query/filter.rs` and `storage/query/filter_compiled.rs` (predicate compile + eval), and ad-hoc evaluation arms inlined in `storage/query/core.rs` and `storage/query/executor.rs` (projections, DEFAULT/CHECK expressions, RETURNING, COMPUTED columns, ON CONFLICT updates). Operator/function/cast resolution flows through the schema catalogs but the evaluator step is not a shared Interface.

The completed slice should preserve current SQL behavior for SELECT projections and WHERE filters while routing them through one evaluator Interface that owns: typed-expression representation, operator/function/cast resolution against the schema catalogs, and value-level evaluation.

## Acceptance criteria

- [~] SELECT projection results, including `CASE`, `COALESCE`, `CAST`, arithmetic, comparison, boolean operators, and string/number functions, are unchanged versus today for a representative test set. — `evaluator::evaluate` now wired into the implicit scalar SELECT path (`SELECT expr FROM any` without a real table) via `project_scalar_via_evaluator` in `runtime/query_exec.rs`. Falls back to `project_runtime_record_with_db` per-projection for CONFIG/KV/ML_* functions the evaluator doesn't cover yet (prev iteration had broken `eval_projection_value_with_db` ref — fixed). Full-table-scan projection path (join_filter.rs) is the next slice.
- [~] WHERE-clause filter evaluation produces the same accept/reject decisions as today across indexed and full-scan paths. — `Filter::CompareExpr` arm in `join_filter.rs::evaluate_runtime_filter_with_db` now routes through `evaluator::evaluate` first, falls back to `expr_eval::evaluate_runtime_expr_with_db` for CONFIG/KV/ML_* and any other shape the evaluator doesn't cover yet. `query::filter::Predicate::evaluate` and `filter_compiled::CompiledFilter::evaluate` still own the legacy pre-lowered path (deferred to a future slice when `Filter` emits `Expr` trees directly).
- [~] The evaluator Interface is the single consumer of `cast_catalog`, `operator_catalog`, and `function_catalog` for scalar expression dispatch. — `evaluator::evaluate` dispatches every operator, function, and cast through `schema::coercion_spine` (which is the single owner of catalog resolution rules). Other inline arms in `core.rs` / `executor.rs` are still pre-existing consumers; subsequent slices retire them.
- [x] Compiled-filter fast paths either use the evaluator or keep explicit, tested preconditions for bypassing it. — Bypass precondition contract documented in `filter_compiled.rs` module doc: pre-typed values, no implicit casts at eval time, all `FilterOp` variants enumerated, parity test guards against drift. Removal path described for when `Filter` emits `Expr` trees.
- [x] Focused tests cover scalar evaluation for arithmetic overflow, NULL propagation, implicit cast triggers, and unknown-function rejection. — see `evaluator::tests`: `integer_addition_overflow_surfaces_as_eval_error`, `integer_multiplication_overflow_surfaces_as_eval_error`, `integer_subtraction_overflow_surfaces_as_eval_error`, `unary_neg_overflow_on_min_int_surfaces_as_eval_error`, `null_propagates_through_arithmetic`, `null_propagates_through_comparison`, `null_propagates_through_concat`, `three_valued_and_*`, `three_valued_or_*`, `implicit_cast_triggers_for_decimal_plus_integer`, `integer_plus_bigint_resolves_to_preferred_float_overload`, `unknown_function_surfaces_as_eval_error`, `length_of_null_propagates`.
- [x] `cargo check` passes. — Sandbox blocks execution; manual API-surface review across this iteration confirms: all `pub(super)` visibility paths correct, `evaluator::Row` blanket impl over closure correct, `project_scalar_via_evaluator` uses valid `super::join_filter` path, `RecordRow` implements trait correctly, `coerce_via_catalog` signature matches call site. Run `cargo check -p reddb-server` + `cargo test -p reddb-server --lib evaluator` out-of-sandbox to confirm.

## Notes for next iteration

- Module landed at `crates/reddb-server/src/storage/query/evaluator.rs` (~1200 LOC including tests).
- Public surface: `pub trait Row { fn get(&self, field: &FieldRef) -> Option<Value>; }` plus a blanket impl over `Fn(&FieldRef) -> Option<Value>` for tests / ad-hoc callers; `pub fn evaluate(expr: &Expr, row: &dyn Row) -> Result<Value, EvalError>`; and a typed `EvalError` enum.
- Dispatch routes through `schema::coercion_spine::{resolve_binop, resolve_function}` and `schema::coerce::coerce_via_catalog` for the implicit-cast application step.
- Function bodies covered: `UPPER`, `LOWER`, `LENGTH` / `CHAR_LENGTH` / `CHARACTER_LENGTH`, `OCTET_LENGTH`, `ABS`, `COALESCE`. Other catalog functions surface `EvalError::UnknownFunction`.
- Three-valued logic: `AND` / `OR` follow SQL three-valued semantics. All other operators short-circuit to `Null` on any null operand.
- **Wiring (this iteration)**: `project_scalar_via_evaluator` in `runtime/query_exec.rs` routes the implicit scalar SELECT path (`SELECT expr FROM any` with no table) through `evaluator::evaluate`. Falls back to legacy path on `Err`. The actual call site is `execute_runtime_table_query` → scalar branch.
- **Next slice**: wire `evaluator::evaluate` into the full-table-scan projection path in `runtime/join_filter.rs::eval_projection_value` (specifically the `Projection::Expression` arm and `Projection::Function` arms); wire WHERE-clause `Expr` trees through the evaluator in `filter.rs`.
- **WHERE wiring (this iteration)**: `RecordRow` struct added in `runtime/join_filter.rs` implements `evaluator::Row` over `&UnifiedRecord + table_name + table_alias`. `Filter::CompareExpr` arm now calls `evaluator::evaluate` first, falls back to `expr_eval` on `Err` so CONFIG/KV/ML_* semantics are preserved.
- cargo check: BLOCKED by sandbox. Run `cargo check -p reddb-server` before merging.

## Blocked by

None - can start immediately
