//! Operator catalog — static table of built-in operator
//! signatures, Fase 3 type-system extension.
//!
//! Parallel to `function_catalog` but scoped to infix /
//! prefix operators. Where `function_catalog` holds one row
//! per function overload (`UPPER(text) -> text`, `ABS(int) ->
//! int`, …), this module holds one row per operator overload
//! (`+(int, int) -> int`, `+(float, float) -> float`, `||(text,
//! text) -> text`, …).
//!
//! Mirrors PG's `pg_operator` with the same simplifications as
//! `function_catalog`:
//!
//! - Name is a `&'static str` symbol ('+', '-', '||', '=', …)
//! - `lhs_type` / `rhs_type` / `return_type` are concrete
//!   `DataType`s; polymorphism (`anyelement`) deferred.
//! - `kind` distinguishes Infix, Prefix, Postfix.
//! - Pointer to the backing scalar function is NOT stored —
//!   the runtime evaluator in `expr_eval.rs` still dispatches
//!   on `BinOp` tags rather than through a function indirection.
//!   That indirection lands when we have a real function-
//!   invocation layer for scalar dispatch.
//!
//! The catalog is used by the Fase 3 typer (`expr_typing.rs`)
//! to resolve `BinaryOp` nodes: instead of the hand-rolled
//! `binop_result_type` match, the typer can walk the catalog
//! with the call-site LHS/RHS types and pick the best overload
//! via `func_select_candidate`-style heuristics.
//!
//! This module is **not yet wired** into the typer. Wiring
//! flips `expr_typing::binop_result_type` to call
//! `operator_catalog::resolve` and falls back to the hand-
//! rolled path only when no catalog entry matches.

use super::types::DataType;

/// Operator position — infix for binary ops, prefix for unary
/// `-` and `NOT`, postfix for legacy SQL shapes we don't have
/// today but reserve for completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorKind {
    Infix,
    Prefix,
    Postfix,
}

/// One row in the static operator catalog.
#[derive(Debug, Clone, Copy)]
pub struct OperatorEntry {
    /// Operator symbol: "+", "-", "*", "/", "%", "||", "=",
    /// "<>", "<", "<=", ">", ">=", "AND", "OR", "NOT".
    pub name: &'static str,
    /// Left-hand operand type. For `Prefix` operators this is
    /// unused and should be `DataType::Nullable` (the "don't
    /// care" marker).
    pub lhs_type: DataType,
    /// Right-hand operand type. Always populated.
    pub rhs_type: DataType,
    /// Result type of the operator.
    pub return_type: DataType,
    /// Infix / Prefix / Postfix.
    pub kind: OperatorKind,
}

const fn infix(
    name: &'static str,
    lhs: DataType,
    rhs: DataType,
    ret: DataType,
) -> OperatorEntry {
    OperatorEntry {
        name,
        lhs_type: lhs,
        rhs_type: rhs,
        return_type: ret,
        kind: OperatorKind::Infix,
    }
}

const fn prefix(name: &'static str, operand: DataType, ret: DataType) -> OperatorEntry {
    OperatorEntry {
        name,
        lhs_type: DataType::Nullable,
        rhs_type: operand,
        return_type: ret,
        kind: OperatorKind::Prefix,
    }
}

/// Static catalog of built-in operators. Grouped by symbol
/// family for readability.
pub const OPERATOR_CATALOG: &[OperatorEntry] = &[
    // ── Arithmetic: + ──
    infix(
        "+",
        DataType::Integer,
        DataType::Integer,
        DataType::Integer,
    ),
    infix("+", DataType::Integer, DataType::Float, DataType::Float),
    infix("+", DataType::Float, DataType::Integer, DataType::Float),
    infix("+", DataType::Float, DataType::Float, DataType::Float),
    infix("+", DataType::BigInt, DataType::BigInt, DataType::BigInt),
    infix(
        "+",
        DataType::Decimal,
        DataType::Decimal,
        DataType::Decimal,
    ),
    // ── Arithmetic: - ──
    infix(
        "-",
        DataType::Integer,
        DataType::Integer,
        DataType::Integer,
    ),
    infix("-", DataType::Float, DataType::Float, DataType::Float),
    infix("-", DataType::BigInt, DataType::BigInt, DataType::BigInt),
    infix(
        "-",
        DataType::Decimal,
        DataType::Decimal,
        DataType::Decimal,
    ),
    // Unary negation — prefix operator.
    prefix("-", DataType::Integer, DataType::Integer),
    prefix("-", DataType::Float, DataType::Float),
    prefix("-", DataType::BigInt, DataType::BigInt),
    prefix("-", DataType::Decimal, DataType::Decimal),
    // ── Arithmetic: * ──
    infix(
        "*",
        DataType::Integer,
        DataType::Integer,
        DataType::Integer,
    ),
    infix("*", DataType::Float, DataType::Float, DataType::Float),
    infix("*", DataType::BigInt, DataType::BigInt, DataType::BigInt),
    // ── Arithmetic: / (always produces Float) ──
    infix("/", DataType::Integer, DataType::Integer, DataType::Float),
    infix("/", DataType::Float, DataType::Float, DataType::Float),
    infix("/", DataType::BigInt, DataType::BigInt, DataType::Float),
    // ── Arithmetic: % (modulo) ──
    infix(
        "%",
        DataType::Integer,
        DataType::Integer,
        DataType::Integer,
    ),
    infix("%", DataType::BigInt, DataType::BigInt, DataType::BigInt),
    // ── String concat: || ──
    infix("||", DataType::Text, DataType::Text, DataType::Text),
    // ── Comparison: = ──
    infix(
        "=",
        DataType::Integer,
        DataType::Integer,
        DataType::Boolean,
    ),
    infix("=", DataType::Float, DataType::Float, DataType::Boolean),
    infix("=", DataType::Text, DataType::Text, DataType::Boolean),
    infix(
        "=",
        DataType::Boolean,
        DataType::Boolean,
        DataType::Boolean,
    ),
    infix("=", DataType::Uuid, DataType::Uuid, DataType::Boolean),
    infix(
        "=",
        DataType::Timestamp,
        DataType::Timestamp,
        DataType::Boolean,
    ),
    // ── Comparison: <> ──
    infix(
        "<>",
        DataType::Integer,
        DataType::Integer,
        DataType::Boolean,
    ),
    infix("<>", DataType::Float, DataType::Float, DataType::Boolean),
    infix("<>", DataType::Text, DataType::Text, DataType::Boolean),
    // ── Ordered comparisons: <, <=, >, >= ──
    infix(
        "<",
        DataType::Integer,
        DataType::Integer,
        DataType::Boolean,
    ),
    infix("<", DataType::Float, DataType::Float, DataType::Boolean),
    infix("<", DataType::Text, DataType::Text, DataType::Boolean),
    infix(
        "<",
        DataType::Timestamp,
        DataType::Timestamp,
        DataType::Boolean,
    ),
    infix(
        "<=",
        DataType::Integer,
        DataType::Integer,
        DataType::Boolean,
    ),
    infix(
        "<=",
        DataType::Float,
        DataType::Float,
        DataType::Boolean,
    ),
    infix("<=", DataType::Text, DataType::Text, DataType::Boolean),
    infix(
        ">",
        DataType::Integer,
        DataType::Integer,
        DataType::Boolean,
    ),
    infix(">", DataType::Float, DataType::Float, DataType::Boolean),
    infix(">", DataType::Text, DataType::Text, DataType::Boolean),
    infix(
        ">=",
        DataType::Integer,
        DataType::Integer,
        DataType::Boolean,
    ),
    infix(
        ">=",
        DataType::Float,
        DataType::Float,
        DataType::Boolean,
    ),
    infix(">=", DataType::Text, DataType::Text, DataType::Boolean),
    // ── Logical: AND / OR ──
    infix(
        "AND",
        DataType::Boolean,
        DataType::Boolean,
        DataType::Boolean,
    ),
    infix(
        "OR",
        DataType::Boolean,
        DataType::Boolean,
        DataType::Boolean,
    ),
    prefix("NOT", DataType::Boolean, DataType::Boolean),
];

/// Look up every overload for a given operator symbol.
/// Returns a `Vec` of static references so the typer can
/// score each candidate without copying.
pub fn lookup(name: &str) -> Vec<&'static OperatorEntry> {
    OPERATOR_CATALOG
        .iter()
        .filter(|e| e.name == name)
        .collect()
}

/// Resolve an operator call to the best-matching overload.
/// Same heuristic as `function_catalog::resolve` but for
/// binary operators:
///
/// 1. Filter by exact name match.
/// 2. Filter by kind (infix / prefix / postfix).
/// 3. Score each overload by counting exact type matches on
///    both operand positions.
/// 4. Tie-break by preferred return type within category.
///
/// For prefix operators the `lhs` parameter is ignored — pass
/// `DataType::Nullable` as a placeholder.
pub fn resolve(
    name: &str,
    kind: OperatorKind,
    lhs: DataType,
    rhs: DataType,
) -> Option<&'static OperatorEntry> {
    let candidates: Vec<&'static OperatorEntry> = OPERATOR_CATALOG
        .iter()
        .filter(|e| e.name == name && e.kind == kind)
        .collect();

    if candidates.is_empty() {
        return None;
    }

    let mut best: Option<(usize, &'static OperatorEntry)> = None;
    for entry in candidates {
        let lhs_match = match kind {
            OperatorKind::Infix => (entry.lhs_type == lhs) as usize,
            OperatorKind::Prefix | OperatorKind::Postfix => 0,
        };
        let rhs_match = (entry.rhs_type == rhs) as usize;
        let score = lhs_match + rhs_match;

        // Reject zero-exact-match candidates unless the operator
        // has only one overload (single entry → always-pick).
        if score == 0 && OPERATOR_CATALOG.iter().filter(|e| e.name == name).count() > 1 {
            continue;
        }

        match best {
            None => best = Some((score, entry)),
            Some((prev_score, prev_entry)) => {
                if score > prev_score
                    || (score == prev_score
                        && entry.return_type.is_preferred()
                        && !prev_entry.return_type.is_preferred())
                {
                    best = Some((score, entry));
                }
            }
        }
    }

    best.map(|(_, entry)| entry)
}
