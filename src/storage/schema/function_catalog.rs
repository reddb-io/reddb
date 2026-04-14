//! Function catalog — static table of built-in scalar / aggregate
//! function signatures used by the Fase 3 expression typer to
//! resolve `Expr::FunctionCall` nodes to a concrete return type.
//!
//! Mirrors PostgreSQL's `pg_proc` catalog with a deliberately
//! narrow row shape: a single (name, arg_types, return_type, kind)
//! entry per overload. Multiple entries may share the same name —
//! the resolver picks the one whose argument types match (after
//! implicit coercion) using the `func_select_candidate` heuristic
//! described in the roadmap (parte 4 of the plan file).
//!
//! The table is `const &[FunctionEntry]` so it lives in the
//! read-only segment and lookups stay cache-friendly. Linear
//! scan is fine for the ~30 entries the catalog covers today.
//! Future weeks can switch to a `HashMap<&'static str, &[…]>`
//! grouped by name when the table grows past ~500 entries.
//!
//! ## Coverage today
//!
//! Aggregates: COUNT, SUM, AVG, MIN, MAX (the five SQL-standard
//! ones). Each has multiple overloads for the numeric category.
//!
//! Scalars covered:
//!
//! - String: UPPER, LOWER, LENGTH, COALESCE
//! - Math:   ABS, ROUND, FLOOR, CEIL
//! - Time:   NOW, CURRENT_TIMESTAMP, CURRENT_DATE, TIME_BUCKET
//! - Geo:    GEO_DISTANCE, GEO_BEARING, HAVERSINE
//! - Misc:   VERIFY_PASSWORD
//!
//! Variadic functions (COALESCE, GREATEST, LEAST, CONCAT) are
//! marked with `variadic: true` and the resolver treats their
//! `arg_types` slice as a description of the *uniform* element
//! type — the catalog can't enumerate every arity, so the typer
//! checks each call-site argument against `arg_types[0]` instead.
//!
//! ## What's NOT in this catalog
//!
//! - User-defined functions (CREATE FUNCTION) — separate runtime
//!   table, queried after the static catalog yields no match.
//! - Polymorphic signatures (anyelement, anyarray) — Fase 3 W4.
//! - Operator functions backing `+`, `-`, `*` — those go in
//!   `pg_operator` equivalent which we haven't built yet.

use super::cast_catalog::can_implicit_cast;
use super::types::DataType;

/// Function classification — affects resolver behavior and
/// downstream planner cost estimation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    /// Pure scalar function: same input → same output, no side
    /// effects, no row-context dependency. The planner is free
    /// to constant-fold or push-down through joins.
    Scalar,
    /// Aggregate function: consumes a Vec of input values, produces
    /// a single output. Only valid in projection lists / HAVING /
    /// window frames.
    Aggregate,
    /// Window function: like aggregate but evaluates over an
    /// ORDER BY frame. Stays in projection lists only.
    Window,
    /// Side-effecting / time-dependent function: NOW(), RANDOM().
    /// The planner cannot cache results across rows.
    Volatile,
}

/// One signature in the static function catalog.
#[derive(Debug, Clone, Copy)]
pub struct FunctionEntry {
    pub name: &'static str,
    pub arg_types: &'static [DataType],
    pub return_type: DataType,
    pub kind: FunctionKind,
    /// When true, the catalog's `arg_types` describes the element
    /// type of a variadic argument list. Resolver matches each
    /// call-site argument against `arg_types[0]` and ignores
    /// arity entirely.
    pub variadic: bool,
}

const fn entry(
    name: &'static str,
    arg_types: &'static [DataType],
    return_type: DataType,
    kind: FunctionKind,
    variadic: bool,
) -> FunctionEntry {
    FunctionEntry {
        name,
        arg_types,
        return_type,
        kind,
        variadic,
    }
}

// ── Argument-list constants used by multiple entries ──
// These are static slices so the FunctionEntry::arg_types pointer
// stays the same across overloads sharing identical signatures.
// The compiler interns the slice symbols so there's no duplicate
// storage even if multiple entries reference the same array.

const ARGS_INT: &[DataType] = &[DataType::Integer];
const ARGS_FLOAT: &[DataType] = &[DataType::Float];
const ARGS_BIGINT: &[DataType] = &[DataType::BigInt];
const ARGS_TEXT: &[DataType] = &[DataType::Text];
const ARGS_TWO_TEXT: &[DataType] = &[DataType::Text, DataType::Text];
const ARGS_TEXT_INT: &[DataType] = &[DataType::Text, DataType::Integer];
const ARGS_TEXT_TWO_INT: &[DataType] = &[DataType::Text, DataType::Integer, DataType::Integer];
const ARGS_NONE: &[DataType] = &[];
const ARGS_TWO_FLOATS: &[DataType] = &[DataType::Float, DataType::Float];
const ARGS_GEO_PAIR: &[DataType] = &[DataType::GeoPoint, DataType::GeoPoint];
const ARGS_FOUR_FLOATS: &[DataType] = &[
    DataType::Float,
    DataType::Float,
    DataType::Float,
    DataType::Float,
];
const ARGS_TIME_BUCKET: &[DataType] = &[DataType::Text, DataType::Timestamp];
const ARGS_VERIFY_PWD: &[DataType] = &[DataType::Password, DataType::Text];

/// The static function catalog. Append-only; removing a row is a
/// breaking change that may invalidate cached plans referencing
/// the function. Each block is grouped by category for readability.
pub const FUNCTION_CATALOG: &[FunctionEntry] = &[
    // ─────────────────────────────────────────────────────────────
    // Aggregate functions
    // ─────────────────────────────────────────────────────────────
    //
    // COUNT(*) and COUNT(col) are both modelled as `Integer →
    // Integer` here; the parser distinguishes the star form via
    // a separate Projection variant so the catalog doesn't need
    // a magic asterisk overload.
    entry(
        "COUNT",
        ARGS_INT,
        DataType::Integer,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "COUNT",
        ARGS_TEXT,
        DataType::Integer,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "COUNT",
        ARGS_FLOAT,
        DataType::Integer,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "SUM",
        ARGS_INT,
        DataType::Integer,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "SUM",
        ARGS_BIGINT,
        DataType::BigInt,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "SUM",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "AVG",
        ARGS_INT,
        DataType::Float,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "AVG",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "MIN",
        ARGS_INT,
        DataType::Integer,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "MIN",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "MIN",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "MAX",
        ARGS_INT,
        DataType::Integer,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "MAX",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "MAX",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "STDDEV",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "VARIANCE",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "GROUP_CONCAT",
        ARGS_TWO_TEXT,
        DataType::Text,
        FunctionKind::Aggregate,
        false,
    ),
    entry(
        "STRING_AGG",
        ARGS_TWO_TEXT,
        DataType::Text,
        FunctionKind::Aggregate,
        false,
    ),
    // ─────────────────────────────────────────────────────────────
    // Scalar — string
    // ─────────────────────────────────────────────────────────────
    entry(
        "UPPER",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "LOWER",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "LENGTH",
        ARGS_TEXT,
        DataType::Integer,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "CHAR_LENGTH",
        ARGS_TEXT,
        DataType::Integer,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "CHARACTER_LENGTH",
        ARGS_TEXT,
        DataType::Integer,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "OCTET_LENGTH",
        ARGS_TEXT,
        DataType::Integer,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "BIT_LENGTH",
        ARGS_TEXT,
        DataType::Integer,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "SUBSTRING",
        ARGS_TWO_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "SUBSTRING",
        ARGS_TEXT_INT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "SUBSTRING",
        ARGS_TEXT_TWO_INT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "SUBSTR",
        ARGS_TEXT_INT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "SUBSTR",
        ARGS_TEXT_TWO_INT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "POSITION",
        ARGS_TWO_TEXT,
        DataType::Integer,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "TRIM",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "TRIM",
        ARGS_TWO_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "LTRIM",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "LTRIM",
        ARGS_TWO_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "RTRIM",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "RTRIM",
        ARGS_TWO_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "BTRIM",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "BTRIM",
        ARGS_TWO_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "CONCAT",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        true,
    ),
    entry(
        "CONCAT_WS",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        true,
    ),
    entry(
        "REVERSE",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "LEFT",
        ARGS_TEXT_INT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "RIGHT",
        ARGS_TEXT_INT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "QUOTE_LITERAL",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        false,
    ),
    // COALESCE is variadic over a uniform element type. The
    // resolver matches each call-site arg against arg_types[0]
    // (any concrete type), and the return type is propagated
    // from the first non-null argument's type at typing time.
    entry(
        "COALESCE",
        ARGS_TEXT,
        DataType::Text,
        FunctionKind::Scalar,
        true,
    ),
    // ─────────────────────────────────────────────────────────────
    // Scalar — math
    // ─────────────────────────────────────────────────────────────
    entry(
        "ABS",
        ARGS_INT,
        DataType::Integer,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "ABS",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "ROUND",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "FLOOR",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "CEIL",
        ARGS_FLOAT,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
    // ─────────────────────────────────────────────────────────────
    // Scalar — time
    // ─────────────────────────────────────────────────────────────
    //
    // NOW / CURRENT_TIMESTAMP / CURRENT_DATE are no-arg volatile
    // scalars that read the wall clock at evaluation time. The
    // planner must not constant-fold them across rows.
    entry(
        "NOW",
        ARGS_NONE,
        DataType::TimestampMs,
        FunctionKind::Volatile,
        false,
    ),
    entry(
        "CURRENT_TIMESTAMP",
        ARGS_NONE,
        DataType::TimestampMs,
        FunctionKind::Volatile,
        false,
    ),
    entry(
        "CURRENT_DATE",
        ARGS_NONE,
        DataType::Date,
        FunctionKind::Volatile,
        false,
    ),
    entry(
        "TIME_BUCKET",
        ARGS_TIME_BUCKET,
        DataType::TimestampMs,
        FunctionKind::Scalar,
        false,
    ),
    // ─────────────────────────────────────────────────────────────
    // Scalar — geo
    // ─────────────────────────────────────────────────────────────
    entry(
        "GEO_DISTANCE",
        ARGS_GEO_PAIR,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "GEO_DISTANCE",
        ARGS_FOUR_FLOATS,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "GEO_BEARING",
        ARGS_FOUR_FLOATS,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "HAVERSINE",
        ARGS_FOUR_FLOATS,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
    entry(
        "VINCENTY",
        ARGS_FOUR_FLOATS,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
    // ─────────────────────────────────────────────────────────────
    // Scalar — security
    // ─────────────────────────────────────────────────────────────
    //
    // VERIFY_PASSWORD takes a hashed Password column + a candidate
    // plaintext Text and returns Boolean. Marked Volatile because
    // the underlying argon2id verify is intentionally slow and the
    // planner should not cache results.
    entry(
        "VERIFY_PASSWORD",
        ARGS_VERIFY_PWD,
        DataType::Boolean,
        FunctionKind::Volatile,
        false,
    ),
    // Two-floats variant used by some places (legacy dual-arg form).
    entry(
        "POWER",
        ARGS_TWO_FLOATS,
        DataType::Float,
        FunctionKind::Scalar,
        false,
    ),
];

/// Look up a function by name, returning the slice of overloads
/// (possibly empty). The resolver walks this slice and picks the
/// best match using its own coercion logic.
pub fn lookup(name: &str) -> Vec<&'static FunctionEntry> {
    FUNCTION_CATALOG
        .iter()
        .filter(|e| e.name.eq_ignore_ascii_case(name))
        .collect()
}

/// Resolve a function call to the best-matching overload. Returns
/// `None` when no overload matches the call-site argument types
/// (after implicit coercion via the cast catalog). The match
/// algorithm is a tiny version of PG `func_select_candidate`:
///
/// 1. Filter overloads by name (case-insensitive).
/// 2. Filter by arity (variadic entries skip this check).
/// 3. Discard overloads with any incompatible argument position.
///    Exact matches win over implicit-cast matches.
/// 4. Tie-break by preferring the overload whose return type is
///    the preferred member of its category (Float over Integer,
///    Text over Blob, etc.).
pub fn resolve(name: &str, arg_types: &[DataType]) -> Option<&'static FunctionEntry> {
    let candidates = lookup(name);
    if candidates.is_empty() {
        return None;
    }

    // Score each candidate.
    let mut best: Option<(usize, &'static FunctionEntry)> = None;
    for entry in candidates {
        // Arity check (skip for variadic).
        if !entry.variadic && entry.arg_types.len() != arg_types.len() {
            continue;
        }
        if entry.variadic && arg_types.is_empty() {
            // Variadic with zero args is degenerate — skip.
            continue;
        }

        let compatible = if entry.variadic {
            if entry.name.eq_ignore_ascii_case("CONCAT")
                || entry.name.eq_ignore_ascii_case("CONCAT_WS")
            {
                true
            } else {
                let target = entry.arg_types[0];
                arg_types
                    .iter()
                    .all(|arg| *arg == target || can_implicit_cast(*arg, target))
            }
        } else {
            entry
                .arg_types
                .iter()
                .zip(arg_types.iter())
                .all(|(target, arg)| *target == *arg || can_implicit_cast(*arg, *target))
        };

        if !compatible {
            continue;
        }

        let score = if entry.variadic {
            let target = entry.arg_types[0];
            if entry.name.eq_ignore_ascii_case("CONCAT")
                || entry.name.eq_ignore_ascii_case("CONCAT_WS")
            {
                arg_types.len()
            } else {
                arg_types.iter().filter(|t| **t == target).count()
            }
        } else {
            entry
                .arg_types
                .iter()
                .zip(arg_types.iter())
                .filter(|(target, arg)| *target == *arg)
                .count()
        };

        match best {
            None => best = Some((score, entry)),
            Some((best_score, best_entry)) => {
                if score > best_score
                    || (score == best_score
                        && entry.return_type.is_preferred()
                        && !best_entry.return_type.is_preferred())
                {
                    best = Some((score, entry));
                }
            }
        }
    }

    best.map(|(_, entry)| entry)
}
