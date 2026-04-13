use std::cmp::Ordering;

use crate::storage::query::engine::binding::Value;

pub(crate) fn partial_compare_values(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Integer(a), Value::Integer(b)) => Some(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Integer(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
        (Value::Float(a), Value::Integer(b)) => a.partial_cmp(&(*b as f64)),
        (Value::Boolean(a), Value::Boolean(b)) => Some(a.cmp(b)),
        (Value::Uri(a), Value::Uri(b)) => Some(a.cmp(b)),
        (Value::Node(a), Value::Node(b)) => Some(a.cmp(b)),
        (Value::Edge(a), Value::Edge(b)) => Some(a.cmp(b)),
        (Value::Null, Value::Null) => Some(Ordering::Equal),
        _ => None,
    }
}

pub(crate) fn total_compare_values(a: &Value, b: &Value) -> Ordering {
    partial_compare_values(a, b).unwrap_or_else(|| value_type_tag(a).cmp(&value_type_tag(b)))
}

pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    matches!(partial_compare_values(a, b), Some(Ordering::Equal))
}

fn value_type_tag(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Boolean(_) => 1,
        Value::Integer(_) => 2,
        Value::Float(_) => 3,
        Value::String(_) => 4,
        Value::Uri(_) => 5,
        Value::Node(_) => 6,
        Value::Edge(_) => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_compare_keeps_numeric_coercion() {
        assert_eq!(
            partial_compare_values(&Value::Integer(10), &Value::Float(10.0)),
            Some(Ordering::Equal)
        );
        assert_eq!(
            partial_compare_values(&Value::Float(1.5), &Value::Integer(2)),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn total_compare_orders_cross_type_values() {
        assert_eq!(
            total_compare_values(&Value::Boolean(true), &Value::String("x".into())),
            Ordering::Less
        );
        assert_eq!(
            total_compare_values(&Value::Node("a".into()), &Value::Edge("a".into())),
            Ordering::Less
        );
    }

    #[test]
    fn equality_matches_supported_value_pairs() {
        assert!(values_equal(&Value::Integer(7), &Value::Float(7.0)));
        assert!(values_equal(&Value::Null, &Value::Null));
        assert!(!values_equal(
            &Value::String("7".into()),
            &Value::Integer(7)
        ));
    }
}
