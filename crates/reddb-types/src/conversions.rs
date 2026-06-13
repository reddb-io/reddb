//! Ergonomic `From<primitive>` conversions into [`Value`].
//!
//! These fluent-API conversions previously lived in
//! `reddb-server`'s `storage::unified::devx::conversions`. Once [`Value`]
//! moved to this keystone crate (ADR 0052) the orphan rule required them to
//! live with the type they construct: both sides — `Value` and the std
//! primitive — must have a local anchor, and that anchor is `Value` here.
//! The bodies are byte-faithful relocations; behaviour is unchanged.

use crate::Value;

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::text(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::text(s)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Integer(n as i64)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Integer(n)
    }
}

impl From<f32> for Value {
    fn from(n: f32) -> Self {
        Value::Float(n as f64)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Float(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Boolean(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_conversions_construct_expected_value_variants() {
        assert_eq!(Value::from("red"), Value::text("red"));
        assert_eq!(Value::from("db".to_string()), Value::text("db"));
        assert_eq!(Value::from(7_i32), Value::Integer(7));
        assert_eq!(Value::from(9_i64), Value::Integer(9));
        assert_eq!(Value::from(1.5_f32), Value::Float(1.5));
        assert_eq!(Value::from(2.25_f64), Value::Float(2.25));
        assert_eq!(Value::from(true), Value::Boolean(true));
    }
}
