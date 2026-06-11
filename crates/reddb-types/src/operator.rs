//! Binary-operator vocabulary.
//!
//! `BinOp` is the syntactic binary-operator enum the query AST emits and
//! the coercion spine keys overload resolution on. It is **coercion
//! vocabulary** as much as it is parser vocabulary: the spine
//! (`coercion_spine::resolve_binop`) cannot resolve an operator overload
//! without it, and that spine lives in this keystone crate (ADR 0052).
//!
//! Re-homing only the spine while leaving `BinOp` in the server would force
//! this crate to depend back on `reddb-server` — the exact cycle ADR 0052
//! exists to prevent. So the operator vocabulary moves here and the query
//! AST (`reddb-server`'s `storage::query::ast`) re-exports it, keeping every
//! existing `ast::BinOp` call-site untouched.
//!
//! The move is byte-faithful: the enum, its variant set, and the
//! `precedence()` table are relocated verbatim from `storage::query::ast`.

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
