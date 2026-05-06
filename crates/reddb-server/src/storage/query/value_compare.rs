use crate::storage::schema::value_codec::type_tag as registry_type_tag;
use crate::storage::schema::Value;
use std::cmp::Ordering;

#[inline]
fn value_type_tag(v: &Value) -> u8 {
    registry_type_tag(v)
}

pub(crate) fn partial_compare_values(a: &Value, b: &Value) -> Option<Ordering> {
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

pub(crate) fn total_compare_values(a: &Value, b: &Value) -> Ordering {
    partial_compare_values(a, b).unwrap_or_else(|| value_type_tag(a).cmp(&value_type_tag(b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::value_codec;

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
}
