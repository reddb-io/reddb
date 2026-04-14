//! Cast catalog — Fase 3 foundation for explicit / implicit type
//! coercion.
//!
//! Mirrors PostgreSQL's `pg_cast` catalog: a static table of allowed
//! source→target type conversions, each with a **context** that
//! controls when the coercion is legal:
//!
//! - `Implicit`  — the resolver may insert this cast without asking
//!   the user (e.g. `int → float` when adding an int to a float).
//! - `Assignment` — allowed only when assigning to a column of the
//!   target type in an INSERT / UPDATE (e.g. `float → int` with
//!   truncation). Not available during expression evaluation.
//! - `Explicit`  — only via a user-written `CAST(expr AS type)` or
//!   `expr::type`.
//!
//! The table is deliberately small for Week 3: just the numeric and
//! string families plus the handful of built-in widening / narrowing
//! paths the existing runtime evaluator already implements. Later
//! weeks will flesh it out to cover dates, network, domain, and
//! user-defined casts (see `CREATE CAST` in the PG docs).
//!
//! The catalog is queried by `find_cast(src, target, context)` which
//! returns `true` when the coercion is legal for the requested
//! context. The expression resolver in Fase 3 analyze will call this
//! to decide whether operator overload candidates are viable.

use super::types::DataType;

/// Context in which a cast is being attempted. Matches the PG
/// `CoercionContext` enum modulo feature gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CastContext {
    /// The cast is being used inside an expression and must be
    /// inserted implicitly by the resolver — `a + b` where `a` is
    /// `int` and `b` is `float` requires an `int → float` implicit
    /// cast on the left operand.
    Implicit,
    /// The cast is being applied because the resolver is about to
    /// assign the value to a target column of a specific type
    /// (INSERT / UPDATE RHS → column). Stricter than Implicit
    /// because assignment-time coercions may lose information
    /// (truncation, saturation).
    Assignment,
    /// The cast is being applied because the user wrote it
    /// explicitly — `CAST(expr AS type)` or `expr::type`. This is
    /// the widest context; most allowed casts include Explicit.
    Explicit,
}

impl CastContext {
    /// Returns `true` when a cast legal in `self` is also legal in
    /// `other`. Used when the catalog entry lists its "minimum"
    /// required context and the resolver asks whether a specific
    /// usage satisfies that minimum.
    ///
    /// Ordering (widest → narrowest): `Explicit ⊇ Assignment ⊇ Implicit`.
    /// An implicit cast can be used anywhere; an explicit-only cast
    /// needs an explicit call site.
    pub fn allows(self, other: CastContext) -> bool {
        use CastContext::*;
        matches!(
            (self, other),
            (Explicit, _)
                | (Assignment, Assignment)
                | (Assignment, Implicit)
                | (Implicit, Implicit)
        )
    }
}

/// One row in the static cast catalog. Equivalent to a PG `pg_cast`
/// tuple modulo the `castfunc` / `castmethod` fields — all reddb
/// built-in casts today go through the `schema::coerce` module, so
/// we only need to record the (src, target, context) triple plus a
/// `lossy` flag that informs diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct CastEntry {
    pub src: DataType,
    pub target: DataType,
    /// Minimum context in which this cast may be applied. Implicit
    /// means "always allowed", Assignment means "INSERT/UPDATE RHS",
    /// Explicit means "CAST(…) only".
    pub context: CastContext,
    /// Whether the cast may lose information (truncation, overflow).
    /// Diagnostics use this to warn users writing lossy implicit
    /// conversions (rare — the table avoids listing lossy casts at
    /// Implicit context on purpose).
    pub lossy: bool,
}

/// Static catalog of built-in casts. Each row is a compile-time
/// constant so lookups are cache-friendly and the whole table lives
/// in the read-only segment. Categories covered today:
///
/// - Numeric widening (int ↔ float ↔ bigint ↔ unsigned)
/// - Numeric narrowing (explicit / assignment only)
/// - Anything → text (display_string path)
/// - text → domain types (email, url, phone, semver, cidr, …)
///   when the string parses cleanly
/// - Boolean ↔ integer
///
/// Adding a row is cheap — just append. Removing a row is a
/// breaking change because existing queries may depend on the cast.
pub const CAST_CATALOG: &[CastEntry] = &[
    // ── Numeric widening (implicit, lossless) ──
    entry(
        DataType::Integer,
        DataType::BigInt,
        CastContext::Implicit,
        false,
    ),
    entry(
        DataType::Integer,
        DataType::Float,
        CastContext::Implicit,
        false,
    ),
    entry(
        DataType::Integer,
        DataType::Decimal,
        CastContext::Implicit,
        false,
    ),
    entry(
        DataType::UnsignedInteger,
        DataType::Integer,
        CastContext::Implicit,
        false,
    ),
    entry(
        DataType::UnsignedInteger,
        DataType::Float,
        CastContext::Implicit,
        false,
    ),
    entry(
        DataType::BigInt,
        DataType::Float,
        CastContext::Implicit,
        false,
    ),
    // ── Numeric narrowing (assignment — may truncate) ──
    entry(
        DataType::Float,
        DataType::Integer,
        CastContext::Assignment,
        true,
    ),
    entry(
        DataType::Float,
        DataType::BigInt,
        CastContext::Assignment,
        true,
    ),
    entry(
        DataType::Float,
        DataType::UnsignedInteger,
        CastContext::Assignment,
        true,
    ),
    entry(
        DataType::Integer,
        DataType::UnsignedInteger,
        CastContext::Assignment,
        true,
    ),
    // ── Any → Text (explicit — every type can be stringified but we
    //     don't want silent string casts in arithmetic expressions) ──
    entry(
        DataType::Integer,
        DataType::Text,
        CastContext::Explicit,
        false,
    ),
    entry(
        DataType::UnsignedInteger,
        DataType::Text,
        CastContext::Explicit,
        false,
    ),
    entry(
        DataType::Float,
        DataType::Text,
        CastContext::Explicit,
        false,
    ),
    entry(
        DataType::Boolean,
        DataType::Text,
        CastContext::Explicit,
        false,
    ),
    entry(
        DataType::Timestamp,
        DataType::Text,
        CastContext::Explicit,
        false,
    ),
    entry(DataType::Date, DataType::Text, CastContext::Explicit, false),
    entry(DataType::Time, DataType::Text, CastContext::Explicit, false),
    entry(DataType::Uuid, DataType::Text, CastContext::Explicit, false),
    entry(
        DataType::IpAddr,
        DataType::Text,
        CastContext::Explicit,
        false,
    ),
    // ── Text → domain validators (explicit — parsing may fail) ──
    entry(
        DataType::Text,
        DataType::Integer,
        CastContext::Explicit,
        true,
    ),
    entry(DataType::Text, DataType::Float, CastContext::Explicit, true),
    entry(
        DataType::Text,
        DataType::Boolean,
        CastContext::Explicit,
        true,
    ),
    entry(DataType::Text, DataType::Email, CastContext::Explicit, true),
    entry(DataType::Text, DataType::Url, CastContext::Explicit, true),
    entry(DataType::Text, DataType::Phone, CastContext::Explicit, true),
    entry(
        DataType::Text,
        DataType::Semver,
        CastContext::Explicit,
        true,
    ),
    entry(DataType::Text, DataType::Cidr, CastContext::Explicit, true),
    entry(DataType::Text, DataType::Date, CastContext::Explicit, true),
    entry(DataType::Text, DataType::Time, CastContext::Explicit, true),
    entry(DataType::Text, DataType::Uuid, CastContext::Explicit, true),
    entry(DataType::Text, DataType::Color, CastContext::Explicit, true),
    entry(
        DataType::Text,
        DataType::IpAddr,
        CastContext::Explicit,
        true,
    ),
    // ── Boolean ↔ Integer (explicit to avoid surprise truth-y casts) ──
    entry(
        DataType::Boolean,
        DataType::Integer,
        CastContext::Explicit,
        false,
    ),
    entry(
        DataType::Integer,
        DataType::Boolean,
        CastContext::Explicit,
        false,
    ),
    // ── Identity casts (implicit, free) ──
    // Listed so the resolver can always find a trivially-valid entry
    // when both sides are the same type — otherwise it would fall
    // through to the "no cast found" error path.
    entry(
        DataType::Integer,
        DataType::Integer,
        CastContext::Implicit,
        false,
    ),
    entry(
        DataType::Float,
        DataType::Float,
        CastContext::Implicit,
        false,
    ),
    entry(DataType::Text, DataType::Text, CastContext::Implicit, false),
    entry(
        DataType::Boolean,
        DataType::Boolean,
        CastContext::Implicit,
        false,
    ),
];

/// Helper for building const catalog entries without `..Default::default()`
/// noise. Const-fn so the whole table stays compile-time.
const fn entry(src: DataType, target: DataType, context: CastContext, lossy: bool) -> CastEntry {
    CastEntry {
        src,
        target,
        context,
        lossy,
    }
}

/// Look up whether a cast from `src` to `target` is legal in the
/// given `ctx`. Returns the matching catalog entry so callers can
/// inspect the `lossy` flag for diagnostics. Identity casts
/// (`src == target`) always succeed with an implicit, lossless
/// synthetic entry if the catalog doesn't list the specific type.
///
/// Lookup is linear across the static table — fine for a 40-entry
/// catalog, and the whole array sits in L1 cache. Future weeks can
/// switch to a hash-backed index if the table grows past ~500 rows.
pub fn find_cast(src: DataType, target: DataType, ctx: CastContext) -> Option<CastEntry> {
    if src == target {
        return Some(CastEntry {
            src,
            target,
            context: CastContext::Implicit,
            lossy: false,
        });
    }
    CAST_CATALOG
        .iter()
        .find(|e| e.src == src && e.target == target && e.context.allows(ctx))
        .copied()
}

/// Returns `true` when `src` can be implicitly coerced to `target`
/// in expression context — the hot path for operator overload
/// resolution. Equivalent to `find_cast(src, target, Implicit).is_some()`
/// but inlines better at call sites.
pub fn can_implicit_cast(src: DataType, target: DataType) -> bool {
    find_cast(src, target, CastContext::Implicit).is_some()
}

/// Returns `true` when `src` can be coerced to `target` for
/// assignment to a column of type `target` — INSERT / UPDATE RHS.
pub fn can_assignment_cast(src: DataType, target: DataType) -> bool {
    find_cast(src, target, CastContext::Assignment).is_some()
}

/// Returns `true` when the user-written `CAST(src AS target)` is
/// allowed. The Explicit context is the widest — anything allowed
/// for Implicit or Assignment is also allowed here.
pub fn can_explicit_cast(src: DataType, target: DataType) -> bool {
    find_cast(src, target, CastContext::Explicit).is_some()
}
