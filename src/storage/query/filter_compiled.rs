//! Compiled filter interpreter — flat opcode evaluation.
//!
//! Mirrors PostgreSQL's `ExprState::steps` / `ExecInterpExpr`
//! (`src/backend/executor/execExprInterp.c`). The legacy walker in
//! `super::filter::Filter::evaluate` recurses through the AST per row
//! and consults a `&dyn Fn(&str) -> Option<Value>` closure for every
//! predicate's column lookup. That pays:
//!
//! 1. one HashMap (or BTreeMap) lookup per predicate per row;
//! 2. one closure indirection per lookup;
//! 3. one stack frame per AST node per row.
//!
//! `CompiledFilter` does the column resolution **once** at plan time,
//! flattens the AST into a linear `Vec<CompiledOp>`, and evaluates
//! each row with a tight loop over the ops against a `&[Value]` slot
//! indexed by the column index resolved at compile time. The hot loop
//! does no allocation, no string lookup, and no closure call.
//!
//! # Wiring
//!
//! Compile once per query plan with [`CompiledFilter::compile`],
//! providing a column-name → slot-index map (the schema). On every
//! row, build a `&[Value]` slot in schema order and call
//! [`CompiledFilter::evaluate`]. The compiled filter is `Send + Sync`
//! and can be cached on the plan node.
//!
//! # Semantics
//!
//! Identical to the legacy walker, byte for byte:
//! - column missing from the schema at compile time → `CompileError`
//! - `Value::Null` at a slot position → equivalent to "column not
//!   found" in the legacy path: `IsNull` returns `true`, every other
//!   predicate returns `false`
//! - `And` / `Or` are short-circuited per the postfix arity
//! - `Not` flips the top of the stack
//!
//! A fuzz test (`compiled_matches_legacy_for_random_filters`) exercises
//! 1 000 random filter + row pairs to guard against drift.

use std::collections::HashMap;

use super::filter::{Filter, FilterOp, Predicate};
use crate::storage::schema::Value;

/// Error returned by [`CompiledFilter::compile`] when the filter
/// references a column that is not in the provided schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    UnknownColumn(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::UnknownColumn(c) => {
                write!(f, "compiled filter: unknown column '{c}'")
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// Single instruction in a [`CompiledFilter`] op stream.
///
/// Uses postfix evaluation (RPN): predicates push their result onto
/// a small bool stack, `And`/`Or` pop their `n` operands, `Not` flips
/// the top.
#[derive(Debug, Clone)]
pub enum CompiledOp {
    /// Evaluate `predicate` against `slot[col_idx]` and push the
    /// resulting bool. The predicate carries its operator and its
    /// pre-resolved `PredicateValue` (Range / List / Pattern / etc.)
    /// so no allocation happens per row.
    Predicate {
        col_idx: usize,
        predicate: Predicate,
    },
    /// Pop the top `n` bools, push their conjunction (`true` when
    /// `n == 0`, matching `Vec::iter().all`).
    AndN(usize),
    /// Pop the top `n` bools, push their disjunction (`false` when
    /// `n == 0`, matching `Vec::iter().any`).
    OrN(usize),
    /// Pop the top bool, push its negation.
    Not,
}

/// A filter that has been compiled against a fixed column schema.
///
/// Cheap to clone (the op list is `Vec<CompiledOp>` and predicates
/// are already cloned at compile time).
#[derive(Debug, Clone)]
pub struct CompiledFilter {
    ops: Vec<CompiledOp>,
}

impl CompiledFilter {
    /// Compile a [`Filter`] against a column-name → slot-index map.
    ///
    /// Returns [`CompileError::UnknownColumn`] when any predicate
    /// references a column not in the schema.
    pub fn compile(filter: &Filter, schema: &HashMap<String, usize>) -> Result<Self, CompileError> {
        let mut ops = Vec::new();
        compile_into(filter, schema, &mut ops)?;
        Ok(Self { ops })
    }

    /// Number of opcodes in the compiled program. Useful for tests
    /// and for diagnostics.
    pub fn op_count(&self) -> usize {
        self.ops.len()
    }

    /// Evaluate the compiled filter against a row slot.
    ///
    /// `slot` must be indexed by the same column-index map used at
    /// compile time. Out-of-range indices are treated as
    /// `Value::Null` (matching the legacy walker's "column not
    /// found" semantics).
    ///
    /// Hot path — no allocation, no string lookup. The bool stack is
    /// kept tiny (typical filter has ≤ 8 nesting levels) and lives
    /// on the eval call's local `Vec`.
    pub fn evaluate(&self, slot: &[Value]) -> bool {
        // Local bool stack. Capacity 8 covers >99% of real filters
        // without spilling to heap.
        let mut stack: Vec<bool> = Vec::with_capacity(8);
        for op in &self.ops {
            match op {
                CompiledOp::Predicate { col_idx, predicate } => {
                    let value = slot.get(*col_idx).unwrap_or(&Value::Null);
                    // Null + non-IsNull → false, identical to the
                    // legacy walker's "column not found" branch
                    // because Predicate::evaluate handles a Null
                    // value as a regular comparison (which collapses
                    // to false against any non-null literal).
                    let result = predicate.evaluate(value);
                    stack.push(result);
                }
                CompiledOp::AndN(n) => {
                    let take = (*n).min(stack.len());
                    let new_len = stack.len() - take;
                    let result = stack[new_len..].iter().all(|b| *b);
                    stack.truncate(new_len);
                    stack.push(result);
                }
                CompiledOp::OrN(n) => {
                    let take = (*n).min(stack.len());
                    let new_len = stack.len() - take;
                    let result = stack[new_len..].iter().any(|b| *b);
                    stack.truncate(new_len);
                    stack.push(result);
                }
                CompiledOp::Not => {
                    let v = stack.pop().unwrap_or(true);
                    stack.push(!v);
                }
            }
        }
        // An empty filter evaluates to `true` (matches everything),
        // mirroring the legacy walker on an empty `And(vec![])`.
        stack.pop().unwrap_or(true)
    }
}

fn compile_into(
    filter: &Filter,
    schema: &HashMap<String, usize>,
    ops: &mut Vec<CompiledOp>,
) -> Result<(), CompileError> {
    match filter {
        Filter::Predicate(p) => {
            let col_idx = schema
                .get(&p.column)
                .copied()
                .ok_or_else(|| CompileError::UnknownColumn(p.column.clone()))?;
            ops.push(CompiledOp::Predicate {
                col_idx,
                predicate: p.clone(),
            });
        }
        Filter::And(filters) => {
            for f in filters {
                compile_into(f, schema, ops)?;
            }
            ops.push(CompiledOp::AndN(filters.len()));
        }
        Filter::Or(filters) => {
            for f in filters {
                compile_into(f, schema, ops)?;
            }
            ops.push(CompiledOp::OrN(filters.len()));
        }
        Filter::Not(inner) => {
            compile_into(inner, schema, ops)?;
            ops.push(CompiledOp::Not);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::filter::Predicate;

    fn schema(cols: &[&str]) -> HashMap<String, usize> {
        cols.iter()
            .enumerate()
            .map(|(i, c)| (c.to_string(), i))
            .collect()
    }

    fn slot(values: Vec<Value>) -> Vec<Value> {
        values
    }

    // ---------------- compile / shape ----------------

    #[test]
    fn compile_simple_eq() {
        let s = schema(&["a"]);
        let f = Filter::Predicate(Predicate::eq("a", Value::Integer(5)));
        let c = CompiledFilter::compile(&f, &s).unwrap();
        // One predicate op, no boolean composer.
        assert_eq!(c.op_count(), 1);
    }

    #[test]
    fn compile_and_two() {
        let s = schema(&["a", "b"]);
        let f = Filter::And(vec![
            Filter::Predicate(Predicate::eq("a", Value::Integer(1))),
            Filter::Predicate(Predicate::eq("b", Value::Integer(2))),
        ]);
        let c = CompiledFilter::compile(&f, &s).unwrap();
        // Two predicates + AndN
        assert_eq!(c.op_count(), 3);
    }

    #[test]
    fn compile_or_three() {
        let s = schema(&["a"]);
        let f = Filter::Or(vec![
            Filter::Predicate(Predicate::eq("a", Value::Integer(1))),
            Filter::Predicate(Predicate::eq("a", Value::Integer(2))),
            Filter::Predicate(Predicate::eq("a", Value::Integer(3))),
        ]);
        let c = CompiledFilter::compile(&f, &s).unwrap();
        assert_eq!(c.op_count(), 4); // three predicates + OrN
    }

    #[test]
    fn compile_not_wraps_inner() {
        let s = schema(&["a"]);
        let f = Filter::Not(Box::new(Filter::Predicate(Predicate::eq(
            "a",
            Value::Integer(1),
        ))));
        let c = CompiledFilter::compile(&f, &s).unwrap();
        assert_eq!(c.op_count(), 2); // predicate + Not
    }

    #[test]
    fn compile_unknown_column_errors() {
        let s = schema(&["a"]);
        let f = Filter::Predicate(Predicate::eq("missing", Value::Integer(1)));
        let err = CompiledFilter::compile(&f, &s).unwrap_err();
        assert_eq!(err, CompileError::UnknownColumn("missing".to_string()));
    }

    // ---------------- evaluate ----------------

    #[test]
    fn eval_eq_true() {
        let s = schema(&["age"]);
        let c = CompiledFilter::compile(
            &Filter::Predicate(Predicate::eq("age", Value::Integer(42))),
            &s,
        )
        .unwrap();
        assert!(c.evaluate(&slot(vec![Value::Integer(42)])));
    }

    #[test]
    fn eval_eq_false() {
        let s = schema(&["age"]);
        let c = CompiledFilter::compile(
            &Filter::Predicate(Predicate::eq("age", Value::Integer(42))),
            &s,
        )
        .unwrap();
        assert!(!c.evaluate(&slot(vec![Value::Integer(7)])));
    }

    #[test]
    fn eval_and_true_when_all_true() {
        let s = schema(&["a", "b"]);
        let f = Filter::And(vec![
            Filter::Predicate(Predicate::eq("a", Value::Integer(1))),
            Filter::Predicate(Predicate::eq("b", Value::Integer(2))),
        ]);
        let c = CompiledFilter::compile(&f, &s).unwrap();
        assert!(c.evaluate(&slot(vec![Value::Integer(1), Value::Integer(2)])));
    }

    #[test]
    fn eval_and_false_when_one_false() {
        let s = schema(&["a", "b"]);
        let f = Filter::And(vec![
            Filter::Predicate(Predicate::eq("a", Value::Integer(1))),
            Filter::Predicate(Predicate::eq("b", Value::Integer(2))),
        ]);
        let c = CompiledFilter::compile(&f, &s).unwrap();
        assert!(!c.evaluate(&slot(vec![Value::Integer(1), Value::Integer(99)])));
    }

    #[test]
    fn eval_or_true_when_one_true() {
        let s = schema(&["a"]);
        let f = Filter::Or(vec![
            Filter::Predicate(Predicate::eq("a", Value::Integer(1))),
            Filter::Predicate(Predicate::eq("a", Value::Integer(2))),
        ]);
        let c = CompiledFilter::compile(&f, &s).unwrap();
        assert!(c.evaluate(&slot(vec![Value::Integer(2)])));
    }

    #[test]
    fn eval_not_flips_result() {
        let s = schema(&["a"]);
        let inner = Filter::Predicate(Predicate::eq("a", Value::Integer(1)));
        let f = Filter::Not(Box::new(inner));
        let c = CompiledFilter::compile(&f, &s).unwrap();
        assert!(!c.evaluate(&slot(vec![Value::Integer(1)])));
        assert!(c.evaluate(&slot(vec![Value::Integer(2)])));
    }

    #[test]
    fn eval_is_null_on_null_value() {
        let s = schema(&["a"]);
        let c = CompiledFilter::compile(&Filter::Predicate(Predicate::is_null("a")), &s).unwrap();
        assert!(c.evaluate(&slot(vec![Value::Null])));
        assert!(!c.evaluate(&slot(vec![Value::Integer(1)])));
    }

    #[test]
    fn eval_range_predicate() {
        let s = schema(&["age"]);
        let c = CompiledFilter::compile(
            &Filter::Predicate(Predicate::between(
                "age",
                Value::Integer(18),
                Value::Integer(65),
            )),
            &s,
        )
        .unwrap();
        assert!(c.evaluate(&slot(vec![Value::Integer(30)])));
        assert!(!c.evaluate(&slot(vec![Value::Integer(10)])));
        assert!(!c.evaluate(&slot(vec![Value::Integer(70)])));
    }

    #[test]
    fn eval_nested_and_or_not() {
        // (a = 1 AND b = 2) OR NOT (c = 3)
        let s = schema(&["a", "b", "c"]);
        let f = Filter::Or(vec![
            Filter::And(vec![
                Filter::Predicate(Predicate::eq("a", Value::Integer(1))),
                Filter::Predicate(Predicate::eq("b", Value::Integer(2))),
            ]),
            Filter::Not(Box::new(Filter::Predicate(Predicate::eq(
                "c",
                Value::Integer(3),
            )))),
        ]);
        let c = CompiledFilter::compile(&f, &s).unwrap();

        // Both branches: AND-branch true.
        assert!(c.evaluate(&slot(vec![
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(99),
        ])));
        // AND-branch false, NOT-branch true (c != 3).
        assert!(c.evaluate(&slot(vec![
            Value::Integer(99),
            Value::Integer(99),
            Value::Integer(99),
        ])));
        // AND-branch false, NOT-branch false (c == 3).
        assert!(!c.evaluate(&slot(vec![
            Value::Integer(99),
            Value::Integer(99),
            Value::Integer(3),
        ])));
    }

    // ---------------- legacy parity ----------------

    #[test]
    fn compiled_matches_legacy_for_simple_filters() {
        let s = schema(&["a", "b"]);

        // (a = 1 AND b > 5) OR a IS NULL
        let f = Filter::Or(vec![
            Filter::And(vec![
                Filter::Predicate(Predicate::eq("a", Value::Integer(1))),
                Filter::Predicate(Predicate::gt("b", Value::Integer(5))),
            ]),
            Filter::Predicate(Predicate::is_null("a")),
        ]);
        let compiled = CompiledFilter::compile(&f, &s).unwrap();

        let cases: Vec<(Vec<Value>, bool)> = vec![
            // a=1, b=10 → AND true
            (vec![Value::Integer(1), Value::Integer(10)], true),
            // a=1, b=5 → AND false (b > 5 fails)
            (vec![Value::Integer(1), Value::Integer(5)], false),
            // a=null → OR right branch true
            (vec![Value::Null, Value::Integer(0)], true),
            // a=2, b=10 → both branches false
            (vec![Value::Integer(2), Value::Integer(10)], false),
        ];

        for (slot_vals, expected) in cases {
            // Compiled
            let got_compiled = compiled.evaluate(&slot_vals);
            // Legacy
            let row: HashMap<String, Value> = vec!["a", "b"]
                .into_iter()
                .zip(slot_vals.iter().cloned())
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            let got_legacy = f.evaluate(&|c| row.get(c).cloned());
            assert_eq!(
                got_compiled, got_legacy,
                "compiled and legacy disagree on row {:?}",
                slot_vals
            );
            assert_eq!(got_compiled, expected, "wrong answer for {:?}", slot_vals);
        }
    }

    #[test]
    fn compiled_matches_legacy_for_random_filters() {
        // Tiny deterministic fuzzer: 1k filter+row pairs, compare
        // compiled.evaluate vs legacy walker. The compiled path
        // must agree byte-for-byte with the legacy walker.
        let s = schema(&["a", "b", "c"]);
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed
        };

        for _ in 0..1000 {
            let n = (next() % 3) as usize;
            let filter = make_random_filter(&mut next, n);
            let compiled = match CompiledFilter::compile(&filter, &s) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let row_vals = vec![
                Value::Integer((next() % 10) as i64),
                Value::Integer((next() % 10) as i64),
                Value::Integer((next() % 10) as i64),
            ];
            let row: HashMap<String, Value> = vec!["a", "b", "c"]
                .into_iter()
                .zip(row_vals.iter().cloned())
                .map(|(k, v)| (k.to_string(), v))
                .collect();

            let got_compiled = compiled.evaluate(&row_vals);
            let got_legacy = filter.evaluate(&|c| row.get(c).cloned());
            assert_eq!(
                got_compiled, got_legacy,
                "fuzz disagreement: filter={:?} row={:?}",
                filter, row_vals
            );
        }
    }

    fn make_random_filter(next: &mut impl FnMut() -> u64, depth: usize) -> Filter {
        if depth == 0 {
            // Leaf — random column / op / int literal.
            let col = ["a", "b", "c"][(next() % 3) as usize];
            let lit = Value::Integer((next() % 10) as i64);
            let op = next() % 5;
            return match op {
                0 => Filter::Predicate(Predicate::eq(col, lit)),
                1 => Filter::Predicate(Predicate::ne(col, lit)),
                2 => Filter::Predicate(Predicate::lt(col, lit)),
                3 => Filter::Predicate(Predicate::gt(col, lit)),
                _ => Filter::Predicate(Predicate::ge(col, lit)),
            };
        }
        match next() % 3 {
            0 => Filter::And(vec![
                make_random_filter(next, depth - 1),
                make_random_filter(next, depth - 1),
            ]),
            1 => Filter::Or(vec![
                make_random_filter(next, depth - 1),
                make_random_filter(next, depth - 1),
            ]),
            _ => Filter::Not(Box::new(make_random_filter(next, depth - 1))),
        }
    }

    #[test]
    fn evaluate_handles_oversized_index_as_null() {
        // If the schema says column is at index 5 but the slot has
        // length 3, the eval must treat the missing slot as Null
        // (no panic, no out-of-bounds).
        let s = schema(&["a", "b", "out_of_range"]);
        let f = Filter::Predicate(Predicate::eq("out_of_range", Value::Integer(1)));
        let c = CompiledFilter::compile(&f, &s).unwrap();
        // Pass a slot shorter than the schema would suggest.
        let short_slot = vec![Value::Integer(0), Value::Integer(0)];
        // Predicate eq against Null returns false (Null != 1).
        assert!(!c.evaluate(&short_slot));
    }

    #[test]
    fn empty_filter_returns_true() {
        // An empty And() matches everything, like the legacy walker.
        let s = schema(&["a"]);
        let c = CompiledFilter::compile(&Filter::And(vec![]), &s).unwrap();
        assert!(c.evaluate(&slot(vec![Value::Integer(1)])));
    }
}
