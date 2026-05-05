## Parent

Parent: #53

## What to build

Extract a deep Type Coercion Spine Module that unifies cast resolution, operator dispatch, and function dispatch under a single Interface. Today `storage/schema/cast_catalog.rs`, `storage/schema/operator_catalog.rs`, `storage/schema/function_catalog.rs`, and `storage/schema/coerce.rs` each expose their own resolution helpers, and callers in `storage/query/expr_typing.rs` plus DML coercion paths in `storage/schema/coerce.rs` recompute the "best applicable cast / operator / function" decision inline.

The completed slice should preserve current SQL behavior (every implicit/assignment/explicit cast outcome and every operator/function resolution outcome stays the same) while concentrating those decisions into one Module Interface consumed by both the scalar expression evaluator and DML write enforcement.

## Acceptance criteria

- [ ] Implicit, assignment, and explicit cast resolution outcomes are unchanged for a representative cross-product of source/target `DataType` pairs.
- [ ] Operator resolution (`+`, `-`, `*`, `/`, `=`, `<`, `||`, etc.) returns the same `OperatorEntry` for the same argument types.
- [ ] Function resolution (`coalesce`, `cast`, math/string functions, time functions) returns the same `FunctionEntry` for the same argument types.
- [ ] DML INSERT/UPDATE coercion paths consume the spine instead of calling `coerce_via_catalog` plus `find_cast` ad hoc.
- [ ] Focused tests cover: numeric promotion ladder, text↔number assignment-cast rejection, function overload selection, and operator NULL propagation.
- [ ] `cargo check` passes.

## Blocked by

- draft-53-scalar-expression-evaluator
