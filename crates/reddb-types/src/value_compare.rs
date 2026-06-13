//! Cross-type [`Value`] comparison (ADR 0053, RQL Phase 2 S4b).
//!
//! Pure value-ordering helpers whose only dependencies — [`Value`] and the
//! [`value_codec`](crate::value_codec) type-tag registry — already live in this
//! crate. Re-homed here as the minimal transitive closure that lets the
//! [`vector_metadata`](crate::vector_metadata) AST leaves keep their inherent
//! comparison methods without a `reddb-server` edge. The server's
//! `storage::query::value_compare` module keeps a re-export shim.

use crate::types::Value;
use crate::value_codec::type_tag as registry_type_tag;
use std::cmp::Ordering;

#[inline]
fn value_type_tag(v: &Value) -> u8 {
    registry_type_tag(v)
}

pub fn partial_compare_values(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        (Value::Integer(a), Value::Integer(b)) => Some(a.cmp(b)),
        (Value::UnsignedInteger(a), Value::UnsignedInteger(b)) => Some(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (Value::Text(a), Value::Text(b)) => Some(a.cmp(b)),
        (Value::Boolean(a), Value::Boolean(b)) => Some(a.cmp(b)),
        (Value::Timestamp(a), Value::Timestamp(b)) => Some(a.cmp(b)),
        (Value::Duration(a), Value::Duration(b)) => Some(a.cmp(b)),
        (Value::Blob(a), Value::Blob(b)) => Some(a.cmp(b)),
        (Value::Uuid(a), Value::Uuid(b)) => Some(a.cmp(b)),
        (Value::Integer(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
        (Value::Float(a), Value::Integer(b)) => a.partial_cmp(&(*b as f64)),
        (Value::UnsignedInteger(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
        (Value::Float(a), Value::UnsignedInteger(b)) => a.partial_cmp(&(*b as f64)),
        (Value::Integer(a), Value::UnsignedInteger(b)) => Some((*a as i128).cmp(&(*b as i128))),
        (Value::UnsignedInteger(a), Value::Integer(b)) => Some((*a as i128).cmp(&(*b as i128))),
        _ => None,
    }
}

pub fn total_compare_values(a: &Value, b: &Value) -> Ordering {
    partial_compare_values(a, b).unwrap_or_else(|| value_type_tag(a).cmp(&value_type_tag(b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value_codec;

    /// Cross-type ordering must consult the registry's tag space, not a
    /// local copy. If a new `Value` variant is added without registering
    /// a tag, the registry returns the same byte as `DataType::to_byte`
    /// and this test still passes — but a parallel hand-rolled table
    /// (deleted in this slice) would have drifted.
    #[test]
    fn type_tag_delegates_to_registry() {
        let samples: &[Value] = &[
            Value::Null,
            Value::Boolean(false),
            Value::Integer(0),
            Value::UnsignedInteger(0),
            Value::Float(0.0),
            Value::text(""),
            Value::Blob(Vec::new()),
            Value::Timestamp(0),
            Value::Duration(0),
            Value::Uuid([0; 16]),
        ];
        for v in samples {
            assert_eq!(value_type_tag(v), value_codec::type_tag(v));
        }
    }

    /// Cross-type orderings between disjoint variants stay total: any
    /// two distinct variants must produce a non-Equal ordering through
    /// the registry-derived tag fallback.
    #[test]
    fn cross_type_total_compare_is_total() {
        let pairs: &[(Value, Value)] = &[
            (Value::Boolean(true), Value::text("x")),
            (Value::Integer(1), Value::Boolean(true)),
            (Value::Null, Value::Boolean(false)),
        ];
        for (a, b) in pairs {
            assert_ne!(total_compare_values(a, b), Ordering::Equal);
        }
    }

    #[test]
    fn same_type_partial_comparisons_cover_registered_orderings() {
        let pairs = [
            (Value::Null, Value::Null, Ordering::Equal),
            (Value::Integer(1), Value::Integer(2), Ordering::Less),
            (
                Value::UnsignedInteger(3),
                Value::UnsignedInteger(2),
                Ordering::Greater,
            ),
            (Value::Float(1.0), Value::Float(1.0), Ordering::Equal),
            (Value::text("a"), Value::text("b"), Ordering::Less),
            (Value::Boolean(false), Value::Boolean(true), Ordering::Less),
            (Value::Timestamp(10), Value::Timestamp(9), Ordering::Greater),
            (Value::Duration(4), Value::Duration(4), Ordering::Equal),
            (
                Value::Blob(vec![1]),
                Value::Blob(vec![1, 2]),
                Ordering::Less,
            ),
            (Value::Uuid([1; 16]), Value::Uuid([2; 16]), Ordering::Less),
        ];

        for (left, right, expected) in pairs {
            assert_eq!(partial_compare_values(&left, &right), Some(expected));
            assert_eq!(total_compare_values(&left, &right), expected);
        }
    }

    #[test]
    fn numeric_cross_type_comparisons_use_numeric_value() {
        let pairs = [
            (Value::Integer(2), Value::Float(2.5), Ordering::Less),
            (Value::Float(3.5), Value::Integer(3), Ordering::Greater),
            (
                Value::UnsignedInteger(4),
                Value::Float(4.0),
                Ordering::Equal,
            ),
            (Value::Float(5.0), Value::UnsignedInteger(6), Ordering::Less),
            (
                Value::Integer(-1),
                Value::UnsignedInteger(1),
                Ordering::Less,
            ),
            (
                Value::UnsignedInteger(9),
                Value::Integer(8),
                Ordering::Greater,
            ),
        ];

        for (left, right, expected) in pairs {
            assert_eq!(partial_compare_values(&left, &right), Some(expected));
        }
    }

    #[test]
    fn nan_partial_compare_falls_back_to_total_tag_order() {
        assert_eq!(
            partial_compare_values(&Value::Float(f64::NAN), &Value::Float(1.0)),
            None
        );
        assert_eq!(
            total_compare_values(&Value::Float(f64::NAN), &Value::Float(1.0)),
            Ordering::Equal
        );
        assert_eq!(
            partial_compare_values(&Value::Json(vec![]), &Value::Json(vec![])),
            None
        );
    }
}
