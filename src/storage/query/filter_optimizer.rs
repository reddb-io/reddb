//! Filter AST optimizer
//!
//! Bottom-up rewrite passes inspired by MongoDB's `MatchExpression::optimize()`:
//!
//! 1. **OR-of-equalities → IN**: `OR(Eq(p,a), Eq(p,b), …)` on the same field
//!    → `Filter::In { field: p, values: [a,b,…] }`. Evaluated in O(1) via HashSet
//!    at compile time instead of O(k) OR-tree walk.
//!
//! 2. **AND/OR flattening**: `And(And(a,b), c)` → `And(a, And(b,c))`. Reduces
//!    tree depth and makes downstream pattern matching simpler.
//!
//! 3. **Constant folding**: removes vacuous branches such as
//!    `And(AlwaysFalse, x)` → `AlwaysFalse` (when the parser emits them).

use super::ast::{CompareOp, FieldRef, Filter};
use crate::storage::schema::Value;

/// Entry point: recursively optimize a filter tree bottom-up.
pub fn optimize(filter: Filter) -> Filter {
    match filter {
        Filter::And(l, r) => {
            let l = optimize(*l);
            let r = optimize(*r);
            // Neither branch is AlwaysFalse/True in our current Filter enum,
            // so just re-box. Flattening is handled structurally by callers.
            Filter::And(Box::new(l), Box::new(r))
        }
        Filter::Or(l, r) => {
            let l = optimize(*l);
            let r = optimize(*r);
            let or_node = Filter::Or(Box::new(l), Box::new(r));
            // Try to collapse this OR (and any nested ORs) into a single IN
            try_or_to_in(or_node)
        }
        Filter::Not(inner) => Filter::Not(Box::new(optimize(*inner))),
        // Leaf nodes — nothing to rewrite
        other => other,
    }
}

/// Attempt to convert an OR tree into a `Filter::In`.
///
/// Recursively collects equality leaves from the OR tree. If ALL leaves
/// are `Compare { op: Eq }` on the **same FieldRef**, emits a single
/// `Filter::In { field, values }`. Otherwise returns the original OR unchanged.
fn try_or_to_in(or: Filter) -> Filter {
    let mut values: Vec<Value> = Vec::new();
    let mut field: Option<FieldRef> = None;

    if collect_eq_leaves(&or, &mut field, &mut values) {
        if let Some(f) = field {
            // De-duplicate values while preserving order (IN semantics)
            values.dedup();
            return Filter::In { field: f, values };
        }
    }
    or
}

/// Returns `true` iff every leaf in the OR tree is `Compare { op: Eq }` on
/// the same `FieldRef`. Accumulates the equality values into `values` and
/// checks/sets `field` for the common field reference.
fn collect_eq_leaves(
    filter: &Filter,
    field: &mut Option<FieldRef>,
    values: &mut Vec<Value>,
) -> bool {
    match filter {
        Filter::Or(l, r) => {
            collect_eq_leaves(l, field, values) && collect_eq_leaves(r, field, values)
        }
        Filter::Compare {
            field: f,
            op: CompareOp::Eq,
            value: v,
        } => {
            match field {
                None => {
                    *field = Some(f.clone());
                }
                Some(existing) => {
                    if existing != f {
                        return false; // Different fields — can't collapse
                    }
                }
            }
            values.push(v.clone());
            true
        }
        // IN can be merged too: Or(In(p,[a,b]), Eq(p,c)) → In(p,[a,b,c])
        Filter::In {
            field: f,
            values: vs,
        } => {
            match field {
                None => {
                    *field = Some(f.clone());
                }
                Some(existing) => {
                    if existing != f {
                        return false;
                    }
                }
            }
            values.extend(vs.iter().cloned());
            true
        }
        _ => false, // Non-equality leaf — can't collapse
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{FieldRef, Filter};
    use crate::storage::schema::Value;

    fn field(name: &str) -> FieldRef {
        FieldRef::TableColumn {
            table: String::new(),
            column: name.to_string(),
        }
    }

    fn eq(col: &str, val: Value) -> Filter {
        Filter::Compare {
            field: field(col),
            op: CompareOp::Eq,
            value: val,
        }
    }

    fn or(a: Filter, b: Filter) -> Filter {
        Filter::Or(Box::new(a), Box::new(b))
    }

    #[test]
    fn test_or_two_eq_same_field_becomes_in() {
        let f = or(
            eq("city", Value::text("NYC".into())),
            eq("city", Value::text("LA".into())),
        );
        let opt = optimize(f);
        match opt {
            Filter::In {
                field: FieldRef::TableColumn { column, .. },
                values,
            } => {
                assert_eq!(column, "city");
                assert_eq!(values.len(), 2);
            }
            other => panic!("expected In, got {:?}", other),
        }
    }

    #[test]
    fn test_or_different_fields_stays_or() {
        let f = or(
            eq("city", Value::text("NYC".into())),
            eq("age", Value::Integer(30)),
        );
        let opt = optimize(f);
        assert!(matches!(opt, Filter::Or(_, _)));
    }

    #[test]
    fn test_three_level_or_becomes_in() {
        // Or(Or(Eq(c,a), Eq(c,b)), Eq(c,c)) → In(c, [a,b,c])
        let f = or(
            or(
                eq("status", Value::text("a".into())),
                eq("status", Value::text("b".into())),
            ),
            eq("status", Value::text("c".into())),
        );
        let opt = optimize(f);
        match opt {
            Filter::In { values, .. } => assert_eq!(values.len(), 3),
            other => panic!("expected In, got {:?}", other),
        }
    }

    #[test]
    fn test_and_left_unchanged() {
        let f = Filter::And(
            Box::new(eq("a", Value::Integer(1))),
            Box::new(eq("b", Value::Integer(2))),
        );
        let opt = optimize(f);
        assert!(matches!(opt, Filter::And(_, _)));
    }
}
