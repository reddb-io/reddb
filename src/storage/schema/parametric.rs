//! Parametric type validators — Phase 4 partial drop.
//!
//! The full Fase 3 plan calls for `DataType::Varchar { max_len }`
//! and `DataType::Decimal { precision, scale }` baked into the
//! `DataType` enum. That migration cascades into hundreds of
//! pattern-match sites and requires cargo verification to land
//! safely. Until that session runs, this module ships the
//! **validators** as standalone functions so the rest of the
//! codebase can use them today via `coerce::coerce_via_catalog`
//! without touching `DataType`.
//!
//! Once the enum migration lands, these functions become the
//! body of the cast-catalog entries for the parametric variants.
//!
//! ## Coverage
//!
//! - `validate_varchar(s, max_len)` — Postgres-strict (rejects
//!   strings longer than `max_len`). Configurable via
//!   `VarcharMode::Truncate` for SQL Server-style behavior.
//! - `validate_decimal(value, precision, scale)` — verifies the
//!   value fits in `precision` total digits with `scale` digits
//!   after the decimal point.
//! - `parse_varchar_modifier`, `parse_decimal_modifier` — pull
//!   `(n)` / `(p, s)` out of the legacy `SqlTypeName` modifiers
//!   so callers don't reinvent the parsing.

use super::types::{SqlTypeName, TypeModifier, Value};

/// VARCHAR length-check policy. Postgres rejects; SQL Server
/// silently truncates. reddb defaults to Postgres-strict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarcharMode {
    Reject,
    Truncate,
}

/// Errors raised by the parametric validators.
#[derive(Debug, Clone)]
pub enum ParametricError {
    /// Input string exceeds VARCHAR's declared `max_len`.
    VarcharOverflow {
        actual: usize,
        max: u32,
    },
    /// Decimal exceeds declared total precision.
    DecimalPrecisionOverflow {
        precision: u8,
        actual_digits: usize,
    },
    /// Decimal scale doesn't match the declared scale (rounded
    /// would lose information).
    DecimalScaleOverflow {
        scale: u8,
        actual_fraction_digits: usize,
    },
    /// Input is not a parsable decimal at all.
    NotADecimal(String),
    /// SqlTypeName modifier list doesn't match the expected
    /// shape for VARCHAR(n) / DECIMAL(p,s).
    BadModifier(String),
}

impl std::fmt::Display for ParametricError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VarcharOverflow { actual, max } => {
                write!(f, "string of length {actual} exceeds VARCHAR({max})")
            }
            Self::DecimalPrecisionOverflow { precision, actual_digits } => {
                write!(
                    f,
                    "decimal with {actual_digits} digits exceeds DECIMAL precision {precision}"
                )
            }
            Self::DecimalScaleOverflow {
                scale,
                actual_fraction_digits,
            } => {
                write!(
                    f,
                    "decimal with {actual_fraction_digits} fractional digits exceeds DECIMAL scale {scale}"
                )
            }
            Self::NotADecimal(input) => write!(f, "`{input}` is not a valid decimal literal"),
            Self::BadModifier(reason) => write!(f, "bad parametric modifier: {reason}"),
        }
    }
}

impl std::error::Error for ParametricError {}

/// Validate a `Value::Text` against a declared VARCHAR length
/// limit. Returns the value unchanged on success, or a coerced
/// truncated copy when `mode == Truncate`.
pub fn validate_varchar(
    value: &Value,
    max_len: u32,
    mode: VarcharMode,
) -> Result<Value, ParametricError> {
    let s = match value {
        Value::Text(s) => s,
        // Non-text values are coerced to text via display first
        // then re-checked. Caller can avoid the round-trip by
        // pre-coercing.
        other => {
            return validate_varchar(
                &Value::Text(other.display_string()),
                max_len,
                mode,
            );
        }
    };
    let len = s.chars().count();
    if (len as u32) <= max_len {
        return Ok(value.clone());
    }
    match mode {
        VarcharMode::Reject => Err(ParametricError::VarcharOverflow {
            actual: len,
            max: max_len,
        }),
        VarcharMode::Truncate => {
            let truncated: String = s.chars().take(max_len as usize).collect();
            Ok(Value::Text(truncated))
        }
    }
}

/// Validate a `Value::Decimal` against declared (precision, scale).
/// `precision` is the maximum total digit count (both sides of the
/// decimal point); `scale` is the maximum fractional digit count.
///
/// reddb's `Value::Decimal` stores a fixed-point i64 with implicit
/// scale = 4 (4 digits of fraction). We round-trip via
/// `display_string()` to count the actual digits, which is the
/// most correct path until `DataType::Decimal { p, s }` lands and
/// we can carry the scale on the value itself.
pub fn validate_decimal(
    value: &Value,
    precision: u8,
    scale: u8,
) -> Result<Value, ParametricError> {
    let s = value.display_string();
    let trimmed = s.trim();
    let body = trimmed.strip_prefix('-').unwrap_or(trimmed);
    let (whole, frac) = match body.split_once('.') {
        Some((w, f)) => (w, f),
        None => (body, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err(ParametricError::NotADecimal(s));
    }
    if !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(ParametricError::NotADecimal(s));
    }
    let total_digits = whole.len() + frac.len();
    let frac_digits = frac.len();
    if total_digits > precision as usize {
        return Err(ParametricError::DecimalPrecisionOverflow {
            precision,
            actual_digits: total_digits,
        });
    }
    if frac_digits > scale as usize {
        return Err(ParametricError::DecimalScaleOverflow {
            scale,
            actual_fraction_digits: frac_digits,
        });
    }
    Ok(value.clone())
}

/// Pull `(n)` out of `VARCHAR(n)`'s SqlTypeName modifiers. Returns
/// `Err` if the modifier list is empty, has more than one element,
/// or the single element isn't a `Number`.
pub fn parse_varchar_modifier(sql_type: &SqlTypeName) -> Result<u32, ParametricError> {
    if sql_type.modifiers.is_empty() {
        // VARCHAR with no length is valid in Postgres (treated as
        // unbounded text). Return a sentinel max so callers know
        // not to enforce a length.
        return Ok(u32::MAX);
    }
    if sql_type.modifiers.len() > 1 {
        return Err(ParametricError::BadModifier(format!(
            "VARCHAR expects 1 modifier, got {}",
            sql_type.modifiers.len()
        )));
    }
    match &sql_type.modifiers[0] {
        TypeModifier::Number(n) => Ok(*n),
        other => Err(ParametricError::BadModifier(format!(
            "VARCHAR length must be a number, got {other:?}"
        ))),
    }
}

/// Pull `(p, s)` out of `DECIMAL(p, s)`'s SqlTypeName modifiers.
/// Returns `(precision, scale)` on success.
pub fn parse_decimal_modifier(sql_type: &SqlTypeName) -> Result<(u8, u8), ParametricError> {
    let mods = &sql_type.modifiers;
    if mods.is_empty() {
        // DECIMAL with no params defaults to (38, 0) per SQL standard.
        return Ok((38, 0));
    }
    if mods.len() > 2 {
        return Err(ParametricError::BadModifier(format!(
            "DECIMAL expects (p) or (p,s), got {} modifiers",
            mods.len()
        )));
    }
    let precision = match &mods[0] {
        TypeModifier::Number(n) => u8::try_from(*n).map_err(|_| {
            ParametricError::BadModifier(format!("DECIMAL precision {n} out of u8 range"))
        })?,
        other => {
            return Err(ParametricError::BadModifier(format!(
                "DECIMAL precision must be a number, got {other:?}"
            )))
        }
    };
    let scale = if let Some(s_mod) = mods.get(1) {
        match s_mod {
            TypeModifier::Number(n) => u8::try_from(*n).map_err(|_| {
                ParametricError::BadModifier(format!("DECIMAL scale {n} out of u8 range"))
            })?,
            other => {
                return Err(ParametricError::BadModifier(format!(
                    "DECIMAL scale must be a number, got {other:?}"
                )))
            }
        }
    } else {
        0
    };
    if scale > precision {
        return Err(ParametricError::BadModifier(format!(
            "DECIMAL scale {scale} cannot exceed precision {precision}"
        )));
    }
    Ok((precision, scale))
}
