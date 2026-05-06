## Parent

Parent: #53

## What to build

Introduce the first vertical slice of a deep Scalar Expression Evaluator Module. Today scalar expression handling is split across `storage/query/expr_typing.rs` (type resolution against a `Scope`), `storage/query/filter.rs` and `storage/query/filter_compiled.rs` (predicate compile + eval), and ad-hoc evaluation arms inlined in `storage/query/core.rs` and `storage/query/executor.rs` (projections, DEFAULT/CHECK expressions, RETURNING, COMPUTED columns, ON CONFLICT updates). Operator/function/cast resolution flows through the schema catalogs but the evaluator step is not a shared Interface.

The completed slice should preserve current SQL behavior for SELECT projections and WHERE filters while routing them through one evaluator Interface that owns: typed-expression representation, operator/function/cast resolution against the schema catalogs, and value-level evaluation.

## Acceptance criteria

- [~] SELECT projection results, including `CASE`, `COALESCE`, `CAST`, arithmetic, comparison, boolean operators, and string/number functions, are unchanged versus today for a representative test set. — `evaluator::evaluate` covers Literal, Column, BinaryOp (Add/Sub/Mul/Div/Mod/Concat/Eq/Ne/Lt/Le/Gt/Ge/And/Or), UnaryOp (Neg/Not), Cast, FunctionCall (UPPER/LOWER/LENGTH family/OCTET_LENGTH/ABS/COALESCE), Case, IsNull, InList, Between. **Wiring into the SELECT projection path in `core.rs` / `executor.rs` is the next slice.**
- [ ] WHERE-clause filter evaluation produces the same accept/reject decisions as today across indexed and full-scan paths. — Not yet rewired. `query::filter::Predicate::evaluate` and `filter_compiled::CompiledFilter::evaluate` still own their hot path.
- [~] The evaluator Interface is the single consumer of `cast_catalog`, `operator_catalog`, and `function_catalog` for scalar expression dispatch. — `evaluator::evaluate` dispatches every operator, function, and cast through `schema::coercion_spine` (which is the single owner of catalog resolution rules). Other inline arms in `core.rs` / `executor.rs` are still pre-existing consumers; subsequent slices retire them.
- [ ] Compiled-filter fast paths either use the evaluator or keep explicit, tested preconditions for bypassing it. — Not yet rewired. The compiled filter still bypasses the evaluator. The bypass is intentional for the hot path but needs a documented precondition contract that lands with the migration in the next slice.
- [x] Focused tests cover scalar evaluation for arithmetic overflow, NULL propagation, implicit cast triggers, and unknown-function rejection. — see `evaluator::tests`: `integer_addition_overflow_surfaces_as_eval_error`, `integer_multiplication_overflow_surfaces_as_eval_error`, `integer_subtraction_overflow_surfaces_as_eval_error`, `unary_neg_overflow_on_min_int_surfaces_as_eval_error`, `null_propagates_through_arithmetic`, `null_propagates_through_comparison`, `null_propagates_through_concat`, `three_valued_and_*`, `three_valued_or_*`, `implicit_cast_triggers_for_decimal_plus_integer`, `integer_plus_bigint_resolves_to_preferred_float_overload`, `unknown_function_surfaces_as_eval_error`, `length_of_null_propagates`.
- [ ] `cargo check` passes. — Not executed in this iteration (sandbox blocked `cargo`). Run `cargo check -p reddb-server` and `cargo test -p reddb-server --lib evaluator` to verify.

## Notes for next iteration

- Module landed at `crates/reddb-server/src/storage/query/evaluator.rs` (~700 LOC including tests). Wired through `query/mod.rs` as `pub mod evaluator;` but no caller has been migrated yet.
- Public surface: `pub trait Row { fn get(&self, field: &FieldRef) -> Option<Value>; }` plus a blanket impl over `Fn(&FieldRef) -> Option<Value>` for tests / ad-hoc callers; `pub fn evaluate(expr: &Expr, row: &dyn Row) -> Result<Value, EvalError>`; and a typed `EvalError` enum.
- Dispatch routes through `schema::coercion_spine::{resolve_binop, resolve_function}` and `schema::coerce::coerce_via_catalog` for the implicit-cast application step. Unknown function names are matched against `FUNCTION_CATALOG` so a null arg doesn't silently swallow typos.
- Function bodies covered today: `UPPER`, `LOWER`, `LENGTH` / `CHAR_LENGTH` / `CHARACTER_LENGTH`, `OCTET_LENGTH`, `ABS`, `COALESCE`. Other catalog functions (ROUND, FLOOR, CEIL, GEO_*, time functions, etc.) resolve cleanly through the spine but the runtime body isn't wired — they currently surface `EvalError::UnknownFunction` from the dispatch fallback. Filling those bodies in is a straightforward follow-up.
- Three-valued logic: `AND` / `OR` follow SQL three-valued semantics (`Null AND false → false`, `Null OR true → true`). All other operators short-circuit to `Null` on any null operand.
- Subqueries / aggregates / window functions are deliberately out of scope. The evaluator covers scalar context only.
- Next slice: migrate `Predicate::evaluate` to call `evaluator::evaluate` for non-equality / non-range predicates, and replace the inline projection arms in `query/core.rs` / `query/executor.rs` with `evaluator::evaluate` calls. The compiled-filter fast path can keep its hand-rolled comparison ops as long as it documents the bypass precondition (predicates whose op is in {Eq, Ne, Lt, Le, Gt, Ge, IsNull, IsNotNull, Between, In}).

## Blocked by

None - can start immediately
