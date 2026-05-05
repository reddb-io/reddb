//! Coercion spine — the single owner of "given these argument
//! types, which catalog overload applies and what implicit casts
//! does the engine need to insert?"
//!
//! Before this module landed, the same decision lived in three
//! places:
//!
//! - `cast_catalog::find_cast` — does (src → target) coerce at
//!   context X?
//! - `operator_catalog::resolve` — exact-match overload pick for
//!   `(op, lhs, rhs)`; no coercion-aware fallback.
//! - `function_catalog::resolve` — overload pick that *did* call
//!   `cast_catalog::can_implicit_cast` inline, but the rule lived
//!   inside the function module so any other consumer reinvented
//!   it.
//!
//! Adding a new type or operator therefore meant editing each
//! catalog's resolver in lockstep — the deletion-test signal that
//! a deep module is needed.
//!
//! `CoercionSpine` lifts those rules onto a single trait object so
//! every catalog becomes a *pure registry* (lookup table) and the
//! coercion decision lives in exactly one place. Adding an
//! integer-family widening edge today only requires extending the
//! cast catalog; the spine picks it up for binary-op resolution
//! and for function overload resolution automatically.
//!
//! ## API surface
//!
//! - `resolve_cast(from, to)` — returns the catalog `CastEntry`
//!   permitting the conversion at `Implicit` context, or `None`.
//! - `resolve_binop(op, lhs, rhs)` — returns the operator overload
//!   plus the per-operand implicit casts the engine must apply
//!   before invoking it.
//! - `resolve_function(name, args)` — returns the function overload
//!   plus the per-argument implicit casts.
//!
//! `OperandCoercions` describes *which* operands need an implicit
//! cast and to what target type. The runtime / planner can then
//! synthesize the correct `CompiledScalar::Cast` nodes (or
//! equivalent) without re-running the resolution logic.

use super::cast_catalog::{find_cast, CastContext, CastEntry};
use super::function_catalog::{FunctionEntry, FUNCTION_CATALOG};
use super::operator_catalog::{OperatorEntry, OperatorKind, OPERATOR_CATALOG};
use super::types::DataType;
use crate::storage::query::ast::BinOp;

/// Implicit coercions the runtime must apply to operands before
/// invoking the resolved overload. `Some(ty)` at slot N means
/// "cast operand N to `ty`"; `None` means "no cast — operand
/// matches the overload's expected type already".
///
/// Length is variable: 2 for binary operators (lhs + rhs), N for
/// function calls. The spine guarantees every entry corresponds to
/// a legal `Implicit`-context cast in the cast catalog.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OperandCoercions {
    pub casts: Vec<Option<DataType>>,
}

impl OperandCoercions {
    /// Build an empty coercion list of the given arity.
    pub fn identity(arity: usize) -> Self {
        Self {
            casts: vec![None; arity],
        }
    }

    /// Returns `true` when no operand needs a cast — the resolved
    /// overload took the call site as-is. Useful diagnostic for
    /// query plans that want to flag implicit conversions.
    pub fn is_identity(&self) -> bool {
        self.casts.iter().all(Option::is_none)
    }

    /// Per-slot accessor.
    pub fn at(&self, idx: usize) -> Option<DataType> {
        self.casts.get(idx).copied().flatten()
    }
}

/// The resolution spine. Today the only implementation is the
/// static built-in spine; a future runtime extension layer will
/// implement this trait for user-defined casts / operators /
/// functions registered through `CREATE CAST`, `CREATE OPERATOR`,
/// `CREATE FUNCTION`.
pub trait CoercionSpine {
    fn resolve_cast(&self, from: DataType, to: DataType) -> Option<&'static CastEntry>;
    fn resolve_binop(
        &self,
        op: BinOp,
        lhs: DataType,
        rhs: DataType,
    ) -> Option<(&'static OperatorEntry, OperandCoercions)>;
    fn resolve_function(
        &self,
        name: &str,
        args: &[DataType],
    ) -> Option<(&'static FunctionEntry, OperandCoercions)>;
}

/// Built-in spine over the static catalogs. Stateless; callers can
/// share a single instance across queries.
#[derive(Debug, Default, Clone, Copy)]
pub struct BuiltinSpine;

impl CoercionSpine for BuiltinSpine {
    fn resolve_cast(&self, from: DataType, to: DataType) -> Option<&'static CastEntry> {
        // Implicit context — what the resolver may insert silently.
        // Walk the static catalog directly so the returned reference
        // points to the constant in the read-only segment (callers
        // can store it without copying).
        if from == to {
            // Identity — there's no static row guaranteed for every
            // type pair, so callers asking for identity get None
            // and should special-case it. The spine surfaces the
            // missing-row asymmetry explicitly rather than
            // synthesizing a temporary entry on the heap.
            return None;
        }
        super::cast_catalog::CAST_CATALOG.iter().find(|e| {
            e.src == from && e.target == to && e.context.allows(CastContext::Implicit)
        })
    }

    fn resolve_binop(
        &self,
        op: BinOp,
        lhs: DataType,
        rhs: DataType,
    ) -> Option<(&'static OperatorEntry, OperandCoercions)> {
        let symbol = binop_symbol(op);
        let kind = OperatorKind::Infix;

        // Pass 1: exact match. Same scoring rule the legacy
        // operator_catalog::resolve used — preserves bit-for-bit
        // semantics for queries the catalog already covered.
        let exact = OPERATOR_CATALOG
            .iter()
            .filter(|e| e.name == symbol && e.kind == kind)
            .find(|e| e.lhs_type == lhs && e.rhs_type == rhs);
        if let Some(entry) = exact {
            return Some((entry, OperandCoercions::identity(2)));
        }

        // Pass 2: implicit-coercion match. Score each candidate by
        // how many operand slots are exact (no cast needed) vs.
        // need an implicit cast. Higher score wins; ties broken by
        // preferred return type, matching the catalog's existing
        // tie-break rule.
        let mut best: Option<(usize, &'static OperatorEntry, OperandCoercions)> = None;
        for entry in OPERATOR_CATALOG
            .iter()
            .filter(|e| e.name == symbol && e.kind == kind)
        {
            let lhs_ok = entry.lhs_type == lhs
                || find_cast(lhs, entry.lhs_type, CastContext::Implicit).is_some();
            let rhs_ok = entry.rhs_type == rhs
                || find_cast(rhs, entry.rhs_type, CastContext::Implicit).is_some();
            if !lhs_ok || !rhs_ok {
                continue;
            }
            let lhs_exact = (entry.lhs_type == lhs) as usize;
            let rhs_exact = (entry.rhs_type == rhs) as usize;
            let score = lhs_exact + rhs_exact;
            let coercions = OperandCoercions {
                casts: vec![
                    if lhs_exact == 1 { None } else { Some(entry.lhs_type) },
                    if rhs_exact == 1 { None } else { Some(entry.rhs_type) },
                ],
            };
            match best {
                None => best = Some((score, entry, coercions)),
                Some((prev_score, prev_entry, _)) => {
                    if score > prev_score
                        || (score == prev_score
                            && entry.return_type.is_preferred()
                            && !prev_entry.return_type.is_preferred())
                    {
                        best = Some((score, entry, coercions));
                    }
                }
            }
        }

        best.map(|(_, e, c)| (e, c))
    }

    fn resolve_function(
        &self,
        name: &str,
        args: &[DataType],
    ) -> Option<(&'static FunctionEntry, OperandCoercions)> {
        let mut best: Option<(usize, &'static FunctionEntry, OperandCoercions)> = None;

        for entry in FUNCTION_CATALOG
            .iter()
            .filter(|e| e.name.eq_ignore_ascii_case(name))
        {
            // Arity check (skip for variadic).
            if !entry.variadic && entry.arg_types.len() != args.len() {
                continue;
            }
            if entry.variadic && args.is_empty() {
                continue;
            }

            // Compatibility + per-arg coercion list.
            let (compatible, coercions, score) = if entry.variadic {
                if entry.name.eq_ignore_ascii_case("CONCAT")
                    || entry.name.eq_ignore_ascii_case("CONCAT_WS")
                {
                    // CONCAT family takes anything; the legacy
                    // resolver scored by argument count to win
                    // over scalar overloads. Spine preserves that
                    // and emits no implicit casts (the runtime
                    // dispatcher stringifies whatever it gets).
                    (true, OperandCoercions::identity(args.len()), args.len())
                } else {
                    let target = entry.arg_types[0];
                    let mut casts = Vec::with_capacity(args.len());
                    let mut ok = true;
                    let mut exact = 0usize;
                    for arg in args {
                        if *arg == target {
                            casts.push(None);
                            exact += 1;
                        } else if find_cast(*arg, target, CastContext::Implicit).is_some() {
                            casts.push(Some(target));
                        } else {
                            ok = false;
                            break;
                        }
                    }
                    (ok, OperandCoercions { casts }, exact)
                }
            } else {
                let mut casts = Vec::with_capacity(args.len());
                let mut ok = true;
                let mut exact = 0usize;
                for (target, arg) in entry.arg_types.iter().zip(args.iter()) {
                    if *target == *arg {
                        casts.push(None);
                        exact += 1;
                    } else if find_cast(*arg, *target, CastContext::Implicit).is_some() {
                        casts.push(Some(*target));
                    } else {
                        ok = false;
                        break;
                    }
                }
                (ok, OperandCoercions { casts }, exact)
            };

            if !compatible {
                continue;
            }

            match best {
                None => best = Some((score, entry, coercions)),
                Some((prev_score, prev_entry, _)) => {
                    if score > prev_score
                        || (score == prev_score
                            && entry.return_type.is_preferred()
                            && !prev_entry.return_type.is_preferred())
                    {
                        best = Some((score, entry, coercions));
                    }
                }
            }
        }

        best.map(|(_, e, c)| (e, c))
    }
}

fn binop_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Concat => "||",
        BinOp::Eq => "=",
        BinOp::Ne => "<>",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "AND",
        BinOp::Or => "OR",
    }
}

// ---------------------------------------------------------------------------
// Module-level helpers — let callers use the built-in spine without
// constructing the struct. Production code uses these; tests can swap
// in a custom impl by holding their own `dyn CoercionSpine`.
// ---------------------------------------------------------------------------

/// Resolve a (src → target) implicit cast against the built-in
/// catalogs. Returns the cast entry when the conversion is legal at
/// `Implicit` context.
pub fn resolve_cast(from: DataType, to: DataType) -> Option<&'static CastEntry> {
    BuiltinSpine.resolve_cast(from, to)
}

/// Resolve a binary operator call against the built-in catalogs.
/// Returns the matching overload plus the implicit casts the engine
/// must apply to lhs / rhs before dispatch. Returns `None` when no
/// overload fits, even after considering implicit coercions.
pub fn resolve_binop(
    op: BinOp,
    lhs: DataType,
    rhs: DataType,
) -> Option<(&'static OperatorEntry, OperandCoercions)> {
    BuiltinSpine.resolve_binop(op, lhs, rhs)
}

/// Resolve a function call against the built-in catalogs. Returns
/// the matching overload plus the implicit casts each argument
/// needs. Returns `None` when no overload fits.
pub fn resolve_function(
    name: &str,
    args: &[DataType],
) -> Option<(&'static FunctionEntry, OperandCoercions)> {
    BuiltinSpine.resolve_function(name, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin: an exact-match binop returns the overload with NO
    /// implicit casts. This is the hot path for typical queries —
    /// `int + int`, `text = text`, etc.
    #[test]
    fn binop_exact_match_emits_identity_coercions() {
        let (entry, coercions) = resolve_binop(BinOp::Add, DataType::Integer, DataType::Integer)
            .expect("int + int must resolve");
        assert_eq!(entry.name, "+");
        assert_eq!(entry.lhs_type, DataType::Integer);
        assert_eq!(entry.rhs_type, DataType::Integer);
        assert_eq!(entry.return_type, DataType::Integer);
        assert!(coercions.is_identity());
    }

    /// Pin: `int + float` is an exact match in the operator catalog
    /// (the catalog explicitly lists every numeric cross-pair for
    /// `+`). Pass-1 returns it with identity coercions — no widening
    /// inserted because the overload accepts the operand pair as-is.
    #[test]
    fn binop_int_plus_float_resolves_exact() {
        let (entry, coercions) = resolve_binop(BinOp::Add, DataType::Integer, DataType::Float)
            .expect("int + float must resolve");
        assert_eq!(entry.lhs_type, DataType::Integer);
        assert_eq!(entry.rhs_type, DataType::Float);
        assert_eq!(entry.return_type, DataType::Float);
        assert!(coercions.is_identity());
    }

    /// Pin: `int + bigint` has no exact overload. Pass-2 widens via
    /// the cast catalog — the preferred-return-type tie-break picks
    /// the `(Integer, Float, Float)` overload (Float is "preferred"
    /// in the numeric category), coercing the BigInt rhs to Float.
    /// This pins the actual resolver behaviour so future catalog
    /// edits that reshuffle priorities surface as test diffs.
    #[test]
    fn binop_int_plus_bigint_widens_to_preferred_float() {
        let (entry, coercions) = resolve_binop(BinOp::Add, DataType::Integer, DataType::BigInt)
            .expect("int + bigint must resolve via widening");
        assert_eq!(entry.return_type, DataType::Float);
        // The lhs slot was Integer — the picked overload accepts
        // Integer, so no cast there. The rhs slot needs BigInt →
        // Float (catalog lists this widening as Implicit).
        assert_eq!(coercions.at(0), None);
        assert_eq!(coercions.at(1), Some(DataType::Float));
    }

    /// Pin: function resolution surfaces the per-argument coercion
    /// list. `LENGTH(text)` resolves with no casts; if a future
    /// caller passes an Integer it should fail — Integer → Text is
    /// not implicit.
    #[test]
    fn function_exact_match_emits_identity() {
        let (entry, coercions) = resolve_function("LENGTH", &[DataType::Text])
            .expect("LENGTH(text) must resolve");
        assert_eq!(entry.name, "LENGTH");
        assert!(coercions.is_identity());
    }

    /// Pin: `LENGTH(integer)` has no Integer overload. The cast
    /// catalog lists Integer → Text (at Explicit context, but the
    /// catalog's `allows` rule treats Explicit as legal everywhere
    /// — see `cast_catalog::CastContext::allows`). So the spine
    /// resolves the call to `LENGTH(text)` with an Integer→Text
    /// coercion on slot 0. This pins the existing legacy behaviour;
    /// tightening `allows` is a separate change.
    #[test]
    fn function_int_to_text_widening_resolves_with_explicit_cast() {
        let (entry, coercions) = resolve_function("LENGTH", &[DataType::Integer])
            .expect("LENGTH(int) currently resolves via Integer->Text widening");
        assert_eq!(entry.arg_types, &[DataType::Text]);
        assert_eq!(coercions.at(0), Some(DataType::Text));
    }

    /// Pin: `ABS(int)` and `ABS(float)` both exist; calling with
    /// Integer must pick the int overload (exact), not the float
    /// one (would require a cast).
    #[test]
    fn function_picks_exact_overload_over_cast_overload() {
        let (entry, coercions) = resolve_function("ABS", &[DataType::Integer])
            .expect("ABS(int) must resolve");
        assert_eq!(entry.return_type, DataType::Integer);
        assert!(coercions.is_identity());
    }

    /// Pin: cast catalog passthrough. Integer → Float is implicit
    /// and lossless; the spine returns the catalog row.
    #[test]
    fn cast_int_to_float_is_implicit() {
        let entry = resolve_cast(DataType::Integer, DataType::Float)
            .expect("int -> float must be implicit");
        assert_eq!(entry.src, DataType::Integer);
        assert_eq!(entry.target, DataType::Float);
        assert!(!entry.lossy);
    }

    /// Pin: Float → Integer is registered at Assignment (lossy
    /// truncation). The cast catalog's existing `allows` rule
    /// treats Assignment-min entries as legal at Implicit too, so
    /// the spine surfaces the entry. This pins the legacy behaviour
    /// — a follow-up that tightens `allows` will flip this to
    /// `is_none`.
    #[test]
    fn cast_float_to_int_currently_resolves_via_assignment_entry() {
        let entry = resolve_cast(DataType::Float, DataType::Integer)
            .expect("Float -> Integer resolves under current allows() rule");
        assert!(entry.lossy);
    }
}
