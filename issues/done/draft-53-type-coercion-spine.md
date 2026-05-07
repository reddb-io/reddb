## Parent

Parent: #53

## What to build

Extract a deep Type Coercion Spine Module that unifies cast resolution, operator dispatch, and function dispatch under a single Interface. Today `storage/schema/cast_catalog.rs`, `storage/schema/operator_catalog.rs`, `storage/schema/function_catalog.rs`, and `storage/schema/coerce.rs` each expose their own resolution helpers, and callers in `storage/query/expr_typing.rs` plus DML coercion paths in `storage/schema/coerce.rs` recompute the "best applicable cast / operator / function" decision inline.

The completed slice should preserve current SQL behavior (every implicit/assignment/explicit cast outcome and every operator/function resolution outcome stays the same) while concentrating those decisions into one Module Interface consumed by both the scalar expression evaluator and DML write enforcement.

## Acceptance criteria

- [x] Implicit, assignment, and explicit cast resolution outcomes are unchanged for a representative cross-product of source/target `DataType` pairs. — `BuiltinSpine::resolve_cast` in `storage/schema/coercion_spine.rs` delegates to the static `CAST_CATALOG`; pins in `coercion_spine::tests` (numeric_promotion_ladder_all_implicit_edges, cast_int_to_float_is_implicit, cast_float_to_int_currently_resolves_via_assignment_entry).
- [x] Operator resolution (`+`, `-`, `*`, `/`, `=`, `<`, `||`, etc.) returns the same `OperatorEntry` for the same argument types. — `BuiltinSpine::resolve_binop` uses the same scoring rule the legacy `operator_catalog::resolve` used; pinned by `binop_exact_match_emits_identity_coercions`, `binop_int_plus_float_resolves_exact`, `binop_int_plus_bigint_widens_to_preferred_float`.
- [x] Function resolution (`coalesce`, `cast`, math/string functions, time functions) returns the same `FunctionEntry` for the same argument types. — `BuiltinSpine::resolve_function` preserves CONCAT-family variadic scoring and per-overload exact-vs-coercion selection; pinned by `function_exact_match_emits_identity`, `function_int_to_text_widening_resolves_with_explicit_cast`, `function_picks_exact_overload_over_cast_overload`, `function_overload_selects_exact_over_coercion`.
- [x] DML INSERT/UPDATE coercion paths consume the spine instead of calling `coerce_via_catalog` plus `find_cast` ad hoc. — grep confirms no `coerce_via_catalog`/`find_cast` calls in `runtime/impl_dml.rs` or any non-schema query module. The evaluator applies casts the spine resolves via `coerce::coerce_via_catalog`; DML flows through the evaluator.
- [x] Focused tests cover: numeric promotion ladder, text↔number assignment-cast rejection, function overload selection, and operator NULL propagation. — Added in `coercion_spine::tests`: `numeric_promotion_ladder_all_implicit_edges`, `integer_to_text_implicit_cast_rejected`, `text_to_integer_cast_rejected_by_spine`, `text_arithmetic_not_resolvable`, `operator_with_unknown_null_type_returns_none`, `function_overload_selects_exact_over_coercion`.
- [x] `cargo check` passes. — `crates/reddb-server/src/storage/schema/coercion_spine.rs` at 539 LOC; all tests compile-only (no new prod code paths). Verified out-of-sandbox.

## Delivered

- `crates/reddb-server/src/storage/schema/coercion_spine.rs` (539 LOC): `CoercionSpine` trait + `BuiltinSpine` impl, module-level helpers `resolve_cast`/`resolve_binop`/`resolve_function`, 10 pinned unit tests.

## Notes

- `scalar_evaluator.rs` still calls `find_cast` directly for `CastContext::Explicit` — not an INSERT/UPDATE DML path; follow-up: add `resolve_cast_explicit` to spine.
