//! Unified Query AST
//!
//! Defines the abstract syntax tree for unified table+graph queries.
//! Supports:
//! - Pure table queries (SELECT ... FROM ...)
//! - Pure graph queries (MATCH (a)-[r]->(b) ...)
//! - Table-graph joins (FROM t JOIN GRAPH ...)
//! - Path queries (PATH FROM ... TO ... VIA ...)
//!
//! # Examples
//!
//! ```text
//! -- Table query
//! SELECT ip, ports FROM hosts WHERE os = 'Linux'
//!
//! -- Graph query
//! MATCH (h:Host)-[:HAS_SERVICE]->(s:Service)
//! WHERE h.ip STARTS WITH '192.168'
//! RETURN h, s
//!
//! -- Join query
//! FROM hosts h
//! JOIN GRAPH (h)-[:HAS_VULN]->(v:Vulnerability) AS g
//! WHERE h.criticality > 7
//! RETURN h.ip, h.hostname, v.cve
//!
//! -- Path query
//! PATH FROM host('192.168.1.1') TO host('10.0.0.1')
//! VIA [:AUTH_ACCESS, :CONNECTS_TO]
//! RETURN path
//! ```

#[path = "builders.rs"]
mod builders;
#[path = "core.rs"]
mod core;

pub use builders::*;
pub use core::*;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

// ============================================================================
// Fase 2 — Expression AST (Week 1 foundation)
//
// Types below are the foundation for the parser v2 rewrite described in
// `/home/cyber/.claude/plans/squishy-mixing-honey.md` (Fase 2). They are
// additive — existing `Filter`, `Projection`, `OrderByClause`, and
// `TableQuery` keep working unchanged. Future weeks migrate those AST
// slots to carry an `Expr` instead of ad-hoc `FieldRef` / `Value` /
// `String` fields so deferred Fase 1 items (1.6 ORDER BY expression,
// 1.7 FROM (SELECT …)) can land without further AST churn.
//
// Design notes:
// - `Expr` is an *untyped* syntactic tree. Semantic resolution — type
//   inference, name resolution, coercion pathway — happens in the
//   `analyze/` pass once it exists (Week 2-3 of Fase 2).
// - `Span` uses the existing `lexer::Position` so errors can point at
//   the original source range without re-tokenising.
// - `BinOp` is a flat enum; precedence lives in the parser, not here.
// ============================================================================

use crate::storage::query::lexer::Position;
use crate::storage::schema::{DataType, Value};

/// Half-open byte / line / column range into the original input string.
/// Both endpoints come from the lexer so downstream passes can re-open
/// the source and print a caret-pointed diagnostic without re-lexing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

impl Span {
    pub fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// A synthetic span marker used when a node is constructed
    /// programmatically rather than parsed from source. Debug diagnostics
    /// should check for this via `is_synthetic()` and suppress location
    /// pointers rather than printing `0:0`.
    pub fn synthetic() -> Self {
        Self::default()
    }

    pub fn is_synthetic(&self) -> bool {
        self.start == Position::default() && self.end == Position::default()
    }
}

/// Syntactic binary operators. Parsed precedence determines grouping;
/// this enum only identifies the operator itself. Comparison and logical
/// operators live alongside arithmetic so a single `Expr::BinaryOp`
/// walker can cover every infix form the parser emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    // String
    Concat,
    // Comparison
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // Logical
    And,
    Or,
}

impl BinOp {
    /// Left-binding precedence for Pratt parsing. Higher = binds tighter.
    /// Mirrors PG gram.y's precedence table for the operators we have.
    pub fn precedence(self) -> u8 {
        match self {
            BinOp::Or => 10,
            BinOp::And => 20,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 30,
            BinOp::Concat => 40,
            BinOp::Add | BinOp::Sub => 50,
            BinOp::Mul | BinOp::Div | BinOp::Mod => 60,
        }
    }
}

/// Unary operators — only the two real unaries SQL actually has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Arithmetic negation: `-expr`
    Neg,
    /// Logical negation: `NOT expr`
    Not,
}

/// The syntactic expression tree. Every node carries a `Span` so
/// semantic errors from the analyze pass can point back at the exact
/// token range. Created by the Fase 2 parser, consumed by the analyzer
/// and (eventually) the planner.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A literal value (number, string, boolean, null).
    Literal { value: Value, span: Span },
    /// Reference to a column (possibly qualified by table / alias).
    Column { field: FieldRef, span: Span },
    /// Query parameter placeholder (`?` or `$n`). Used by prepared
    /// statements in Fase 4 — the plan cache strips these so repeated
    /// bindings reuse the same plan.
    Parameter { index: usize, span: Span },
    /// Binary infix operator: `lhs <op> rhs`.
    BinaryOp {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    /// Prefix unary operator.
    UnaryOp {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    /// `CAST(expr AS type)` / `expr::type`.
    Cast {
        inner: Box<Expr>,
        target: DataType,
        span: Span,
    },
    /// Function / aggregate call.
    FunctionCall {
        name: String,
        args: Vec<Expr>,
        span: Span,
    },
    /// `CASE WHEN cond THEN val [...] [ELSE val] END`.
    Case {
        branches: Vec<(Expr, Expr)>,
        else_: Option<Box<Expr>>,
        span: Span,
    },
    /// `IS NULL` / `IS NOT NULL`. Kept as a distinct variant because
    /// SQL treats them as unary postfix operators with special
    /// three-valued semantics.
    IsNull {
        operand: Box<Expr>,
        negated: bool,
        span: Span,
    },
    /// `expr IN (v1, v2, …)`. The rhs list is `Vec<Expr>` — at Week 1
    /// only literal lists survive analyze; correlated subquery lists
    /// land in Week 3 alongside the `Subquery` variant below.
    InList {
        target: Box<Expr>,
        values: Vec<Expr>,
        negated: bool,
        span: Span,
    },
    /// `expr BETWEEN low AND high` — first-class so pushdown can
    /// recognise range predicates without decomposing to `>=` and `<=`.
    Between {
        target: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
        negated: bool,
        span: Span,
    },
    // NOTE: Subquery variants (scalar / value-list / EXISTS) land in
    // Fase 2 Week 3. QueryExpr does not derive PartialEq today and
    // adding it would cascade into half the planner state — out of
    // scope for the Week 1 foundation commit. The parser falls back
    // to the legacy Filter::Compare paths until then.
}

impl Expr {
    /// Extract the span of this expression. Synthetic nodes return
    /// `Span::synthetic()` — callers that need a real location should
    /// check `span.is_synthetic()` before rendering diagnostics.
    pub fn span(&self) -> Span {
        match self {
            Expr::Literal { span, .. }
            | Expr::Column { span, .. }
            | Expr::Parameter { span, .. }
            | Expr::BinaryOp { span, .. }
            | Expr::UnaryOp { span, .. }
            | Expr::Cast { span, .. }
            | Expr::FunctionCall { span, .. }
            | Expr::Case { span, .. }
            | Expr::IsNull { span, .. }
            | Expr::InList { span, .. }
            | Expr::Between { span, .. } => *span,
        }
    }

    /// Constructor shortcut for the common `Literal` case.
    pub fn lit(value: Value) -> Self {
        Expr::Literal {
            value,
            span: Span::synthetic(),
        }
    }

    /// Constructor shortcut for the common `Column` case.
    pub fn col(field: FieldRef) -> Self {
        Expr::Column {
            field,
            span: Span::synthetic(),
        }
    }

    /// Convenience: build a binary operation with a synthetic span.
    /// Used by unit tests and by the Projection → Expr shim while the
    /// migration is in flight.
    pub fn binop(op: BinOp, lhs: Expr, rhs: Expr) -> Self {
        Expr::BinaryOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span: Span::synthetic(),
        }
    }
}

#[cfg(test)]
mod expr_tests {
    use super::*;

    #[test]
    fn precedence_orders_mul_over_add_and_and_over_or() {
        // Higher precedence binds tighter — classic `a OR b AND c` trap.
        assert!(BinOp::Mul.precedence() > BinOp::Add.precedence());
        assert!(BinOp::Add.precedence() > BinOp::Eq.precedence());
        assert!(BinOp::Eq.precedence() > BinOp::And.precedence());
        assert!(BinOp::And.precedence() > BinOp::Or.precedence());
    }

    #[test]
    fn span_synthetic_round_trip() {
        let s = Span::synthetic();
        assert!(s.is_synthetic());
        let real = Span::new(Position::new(1, 1, 0), Position::new(1, 5, 4));
        assert!(!real.is_synthetic());
    }

    #[test]
    fn expr_constructors_carry_synthetic_span() {
        let lit = Expr::lit(Value::Integer(42));
        assert!(lit.span().is_synthetic());
        assert_eq!(
            lit,
            Expr::Literal {
                value: Value::Integer(42),
                span: Span::synthetic(),
            }
        );
    }

    #[test]
    fn binop_shortcut_nests() {
        // a + b * c parses to Add(a, Mul(b, c)) under normal precedence
        let expr = Expr::binop(
            BinOp::Add,
            Expr::col(FieldRef::column("", "a")),
            Expr::binop(
                BinOp::Mul,
                Expr::col(FieldRef::column("", "b")),
                Expr::col(FieldRef::column("", "c")),
            ),
        );
        match expr {
            Expr::BinaryOp {
                op: BinOp::Add,
                rhs,
                ..
            } => match *rhs {
                Expr::BinaryOp { op: BinOp::Mul, .. } => {}
                other => panic!("expected Mul on rhs, got {:?}", other),
            },
            other => panic!("expected Add at root, got {:?}", other),
        }
    }
}
