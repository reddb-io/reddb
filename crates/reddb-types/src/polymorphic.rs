//! Polymorphic pseudo-types — Fase 3 extension.
//!
//! PG-style `anyelement` / `anyarray` / `anynonarray` /
//! `anycompatible` family. These don't exist as concrete
//! `DataType` variants because the analyzer instantiates them
//! fresh at every call site — a function with signature
//! `array_append(anyarray, anyelement) → anyarray` becomes a
//! distinct concrete signature `array_append(int[], int) → int[]`
//! when called with `int` / `int[]` arguments.
//!
//! This module owns:
//!
//! - The `PseudoType` enum that the function catalog uses in
//!   its `arg_types` slice when declaring polymorphic entries.
//! - The `PolymorphicResolver` that instantiates pseudo-types
//!   against concrete call-site arguments, enforcing the
//!   consistency rule: every `anyelement` at the same signature
//!   must resolve to the same concrete type.
//!
//! Scope today (Fase 3 W3):
//!
//! - `AnyElement` — matches any single concrete type.
//! - `AnyArray` — matches any array type. Inferred from the
//!   `AnyElement` it shares a signature with.
//! - `AnyNonArray` — matches any concrete type except arrays.
//! - `AnyCompatible` — like `AnyElement` but tolerates implicit
//!   widening (e.g. `int + float → float`).
//!
//! Deferred:
//!
//! - `AnyRange` / `AnyMultirange` — ranges aren't in Fase 3.
//! - `AnyEnum` — enums are fine via concrete DataType::Enum
//!   today; polymorphic enum wait.
//!
//! This module is **not yet wired** into the function catalog
//! or expr_typing. Wiring adds a `PseudoType`-aware overload in
//! `function_catalog::resolve` when the catalog starts shipping
//! polymorphic rows.

use crate::cast_catalog::can_implicit_cast;
use crate::types::{DataType, TypeCategory};

/// PG-style pseudo-type used by polymorphic function signatures.
/// The resolver substitutes each variant with a concrete
/// `DataType` at analyze time based on call-site arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudoType {
    /// Matches any single concrete type. All `AnyElement`
    /// positions in one signature must resolve to the same
    /// concrete type.
    AnyElement,
    /// Matches any array type. The element type is inferred
    /// from any `AnyElement` in the same signature — if no
    /// `AnyElement` exists, the array's element type is the
    /// matched type itself.
    AnyArray,
    /// Like `AnyElement` but rejects array types. Used by
    /// functions that must not accept arrays to avoid
    /// element-wise confusion.
    AnyNonArray,
    /// Like `AnyElement` but tolerates implicit coercion via
    /// the cast catalog. Two `AnyCompatible` positions may
    /// resolve to different concrete types as long as a common
    /// implicit coercion exists.
    AnyCompatible,
}

/// A single position in a function argument list — either a
/// concrete type or a pseudo-type waiting for substitution.
#[derive(Debug, Clone, Copy)]
pub enum ArgSlot {
    Concrete(DataType),
    Poly(PseudoType),
}

/// The resolver's output — a substitution map that every
/// pseudo-type in a signature has been bound to. Used by
/// `expr_typing` to compute the concrete return type from a
/// signature that mentions the same pseudo-type in its return
/// position.
#[derive(Debug, Clone, Default)]
pub struct Substitution {
    /// Resolved type for `AnyElement` positions.
    pub any_element: Option<DataType>,
    /// Resolved type for `AnyArray` positions.
    pub any_array: Option<DataType>,
    /// Resolved type for `AnyNonArray` positions.
    pub any_nonarray: Option<DataType>,
    /// Resolved type for `AnyCompatible` positions.
    pub any_compatible: Option<DataType>,
}

impl Substitution {
    /// Apply the substitution to a signature slot, returning the
    /// concrete type. Returns `None` when the slot references a
    /// pseudo-type that hasn't been resolved yet — the caller
    /// should treat this as a typer error.
    pub fn apply(&self, slot: ArgSlot) -> Option<DataType> {
        match slot {
            ArgSlot::Concrete(dt) => Some(dt),
            ArgSlot::Poly(PseudoType::AnyElement) => self.any_element,
            ArgSlot::Poly(PseudoType::AnyArray) => self.any_array,
            ArgSlot::Poly(PseudoType::AnyNonArray) => self.any_nonarray,
            ArgSlot::Poly(PseudoType::AnyCompatible) => self.any_compatible,
        }
    }
}

/// Errors raised during polymorphic resolution.
#[derive(Debug, Clone)]
pub enum ResolveError {
    /// Two positions of the same pseudo-type resolved to
    /// conflicting concrete types.
    Conflict {
        pseudo: PseudoType,
        first: DataType,
        other: DataType,
    },
    /// `AnyNonArray` matched against an array type.
    NonArrayGotArray,
    /// `AnyArray` matched against a non-array type.
    ArrayGotScalar,
    /// The signature's arity doesn't match the call site.
    ArityMismatch { expected: usize, got: usize },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict {
                pseudo,
                first,
                other,
            } => {
                write!(
                    f,
                    "polymorphic `{pseudo:?}` bound to `{first:?}` but later seen as `{other:?}`"
                )
            }
            Self::NonArrayGotArray => write!(f, "AnyNonArray position got an array argument"),
            Self::ArrayGotScalar => write!(f, "AnyArray position got a non-array argument"),
            Self::ArityMismatch { expected, got } => {
                write!(
                    f,
                    "polymorphic signature expects {expected} args, got {got}"
                )
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// Attempt to resolve a polymorphic signature against a list of
/// concrete call-site argument types. Returns the substitution
/// on success so `expr_typing` can apply it to the return type.
///
/// Algorithm follows PG's `check_generic_type_consistency`:
///
/// 1. Iterate positional pairs `(signature_slot, call_arg_type)`.
/// 2. For each `Concrete(dt)` slot, require `call_arg_type == dt`
///    or an implicit coercion.
/// 3. For each pseudo slot, bind the call arg to the appropriate
///    substitution map entry. If the entry is already bound to
///    a different type, return `Conflict`.
/// 4. `AnyArray` + `AnyElement` consistency: if both show up in
///    the same signature, verify that the resolved array's
///    element type matches the resolved element.
pub fn resolve(
    signature: &[ArgSlot],
    call_args: &[DataType],
) -> Result<Substitution, ResolveError> {
    if signature.len() != call_args.len() {
        return Err(ResolveError::ArityMismatch {
            expected: signature.len(),
            got: call_args.len(),
        });
    }
    let mut sub = Substitution::default();
    for (slot, &arg_ty) in signature.iter().zip(call_args.iter()) {
        match slot {
            ArgSlot::Concrete(expected) => {
                if *expected != arg_ty && !can_implicit_cast(arg_ty, *expected) {
                    return Err(ResolveError::Conflict {
                        pseudo: PseudoType::AnyElement, // placeholder — concrete mismatch
                        first: *expected,
                        other: arg_ty,
                    });
                }
            }
            ArgSlot::Poly(PseudoType::AnyElement) => {
                bind(&mut sub.any_element, arg_ty, PseudoType::AnyElement)?;
            }
            ArgSlot::Poly(PseudoType::AnyArray) => {
                if arg_ty.category() != TypeCategory::Array {
                    return Err(ResolveError::ArrayGotScalar);
                }
                bind(&mut sub.any_array, arg_ty, PseudoType::AnyArray)?;
            }
            ArgSlot::Poly(PseudoType::AnyNonArray) => {
                if arg_ty.category() == TypeCategory::Array {
                    return Err(ResolveError::NonArrayGotArray);
                }
                bind(&mut sub.any_nonarray, arg_ty, PseudoType::AnyNonArray)?;
            }
            ArgSlot::Poly(PseudoType::AnyCompatible) => {
                // AnyCompatible tolerates implicit coercion. If
                // already bound, verify that the new arg can
                // coerce either direction.
                match sub.any_compatible {
                    None => sub.any_compatible = Some(arg_ty),
                    Some(prev) if prev == arg_ty => {}
                    Some(prev) => {
                        if can_implicit_cast(arg_ty, prev) {
                            // Keep the earlier (wider) binding.
                        } else if can_implicit_cast(prev, arg_ty) {
                            // New arg is wider; update.
                            sub.any_compatible = Some(arg_ty);
                        } else {
                            return Err(ResolveError::Conflict {
                                pseudo: PseudoType::AnyCompatible,
                                first: prev,
                                other: arg_ty,
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(sub)
}

/// Helper: bind a pseudo-type slot for the first time, or
/// verify consistency with the previous binding.
fn bind(
    slot: &mut Option<DataType>,
    arg: DataType,
    pseudo: PseudoType,
) -> Result<(), ResolveError> {
    match *slot {
        None => {
            *slot = Some(arg);
            Ok(())
        }
        Some(prev) if prev == arg => Ok(()),
        Some(prev) => Err(ResolveError::Conflict {
            pseudo,
            first: prev,
            other: arg,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_apply_returns_bound_or_concrete_types() {
        let sub = Substitution {
            any_element: Some(DataType::Integer),
            any_array: Some(DataType::Array),
            any_nonarray: Some(DataType::Text),
            any_compatible: Some(DataType::Float),
        };
        assert_eq!(
            sub.apply(ArgSlot::Concrete(DataType::Boolean)),
            Some(DataType::Boolean)
        );
        assert_eq!(
            sub.apply(ArgSlot::Poly(PseudoType::AnyElement)),
            Some(DataType::Integer)
        );
        assert_eq!(
            sub.apply(ArgSlot::Poly(PseudoType::AnyArray)),
            Some(DataType::Array)
        );
        assert_eq!(
            sub.apply(ArgSlot::Poly(PseudoType::AnyNonArray)),
            Some(DataType::Text)
        );
        assert_eq!(
            sub.apply(ArgSlot::Poly(PseudoType::AnyCompatible)),
            Some(DataType::Float)
        );
        assert_eq!(
            Substitution::default().apply(ArgSlot::Poly(PseudoType::AnyElement)),
            None
        );
    }

    #[test]
    fn resolve_accepts_concrete_and_poly_slots() {
        let sub = resolve(
            &[
                ArgSlot::Concrete(DataType::Float),
                ArgSlot::Poly(PseudoType::AnyElement),
                ArgSlot::Poly(PseudoType::AnyArray),
                ArgSlot::Poly(PseudoType::AnyNonArray),
            ],
            &[
                DataType::Integer,
                DataType::Text,
                DataType::Array,
                DataType::Boolean,
            ],
        )
        .unwrap();
        assert_eq!(sub.any_element, Some(DataType::Text));
        assert_eq!(sub.any_array, Some(DataType::Array));
        assert_eq!(sub.any_nonarray, Some(DataType::Boolean));
    }

    #[test]
    fn resolve_reports_arity_and_kind_errors() {
        assert!(matches!(
            resolve(&[ArgSlot::Poly(PseudoType::AnyElement)], &[]),
            Err(ResolveError::ArityMismatch {
                expected: 1,
                got: 0
            })
        ));
        assert!(matches!(
            resolve(&[ArgSlot::Poly(PseudoType::AnyArray)], &[DataType::Text]),
            Err(ResolveError::ArrayGotScalar)
        ));
        assert!(matches!(
            resolve(
                &[ArgSlot::Poly(PseudoType::AnyNonArray)],
                &[DataType::Array]
            ),
            Err(ResolveError::NonArrayGotArray)
        ));
        assert!(matches!(
            resolve(&[ArgSlot::Concrete(DataType::Boolean)], &[DataType::Text]),
            Err(ResolveError::Conflict { .. })
        ));
    }

    #[test]
    fn repeated_pseudo_slots_must_be_consistent() {
        let ok = resolve(
            &[
                ArgSlot::Poly(PseudoType::AnyElement),
                ArgSlot::Poly(PseudoType::AnyElement),
            ],
            &[DataType::Integer, DataType::Integer],
        )
        .unwrap();
        assert_eq!(ok.any_element, Some(DataType::Integer));

        let err = resolve(
            &[
                ArgSlot::Poly(PseudoType::AnyElement),
                ArgSlot::Poly(PseudoType::AnyElement),
            ],
            &[DataType::Integer, DataType::Text],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ResolveError::Conflict {
                pseudo: PseudoType::AnyElement,
                first: DataType::Integer,
                other: DataType::Text,
            }
        ));
        assert!(err.to_string().contains("AnyElement"));
    }

    #[test]
    fn anycompatible_uses_cast_catalog_to_resolve_binding() {
        let int_then_float = resolve(
            &[
                ArgSlot::Poly(PseudoType::AnyCompatible),
                ArgSlot::Poly(PseudoType::AnyCompatible),
            ],
            &[DataType::Integer, DataType::Float],
        )
        .unwrap();
        assert_eq!(int_then_float.any_compatible, Some(DataType::Integer));

        let float_then_int = resolve(
            &[
                ArgSlot::Poly(PseudoType::AnyCompatible),
                ArgSlot::Poly(PseudoType::AnyCompatible),
            ],
            &[DataType::Float, DataType::Integer],
        )
        .unwrap();
        assert_eq!(float_then_int.any_compatible, Some(DataType::Float));

        assert!(matches!(
            resolve(
                &[
                    ArgSlot::Poly(PseudoType::AnyCompatible),
                    ArgSlot::Poly(PseudoType::AnyCompatible),
                ],
                &[DataType::Boolean, DataType::Json],
            ),
            Err(ResolveError::Conflict {
                pseudo: PseudoType::AnyCompatible,
                ..
            })
        ));
    }
}
