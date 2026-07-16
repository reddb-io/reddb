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
        (Value::Decimal(a), Value::Decimal(b)) => Some(a.cmp(b)),
        (Value::DecimalText(a), Value::DecimalText(b)) => compare_decimal_text(a, b),
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
        (Value::Decimal(a), other) => {
            compare_decimal_text(&format_scaled_i64(*a, 4), &numeric_value_text(other)?)
        }
        (other, Value::Decimal(b)) => {
            compare_decimal_text(&numeric_value_text(other)?, &format_scaled_i64(*b, 4))
        }
        (Value::DecimalText(a), other) => compare_decimal_text(a, &numeric_value_text(other)?),
        (other, Value::DecimalText(b)) => compare_decimal_text(&numeric_value_text(other)?, b),
        _ => None,
    }
}

pub fn total_compare_values(a: &Value, b: &Value) -> Ordering {
    partial_compare_values(a, b).unwrap_or_else(|| value_type_tag(a).cmp(&value_type_tag(b)))
}

fn numeric_value_text(value: &Value) -> Option<String> {
    match value {
        Value::Integer(n) => Some(n.to_string()),
        Value::UnsignedInteger(n) => Some(n.to_string()),
        Value::Float(n) if n.is_finite() => Some(n.to_string()),
        Value::Decimal(n) => Some(format_scaled_i64(*n, 4)),
        Value::DecimalText(n) => Some(n.clone()),
        _ => None,
    }
}

fn format_scaled_i64(value: i64, scale: usize) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.unsigned_abs();
    let factor = 10u64.pow(scale as u32);
    let whole = abs / factor;
    let frac = abs % factor;
    format!("{sign}{whole}.{frac:0scale$}")
}

fn compare_decimal_text(left: &str, right: &str) -> Option<Ordering> {
    let left = ParsedDecimal::parse(left)?;
    let right = ParsedDecimal::parse(right)?;
    Some(left.cmp(&right))
}

#[derive(Debug, Eq, PartialEq)]
struct ParsedDecimal {
    negative: bool,
    digits: String,
    scale: i32,
}

impl ParsedDecimal {
    fn parse(input: &str) -> Option<Self> {
        let mut s = input.trim();
        let mut negative = false;
        if let Some(rest) = s.strip_prefix('-') {
            negative = true;
            s = rest;
        } else if let Some(rest) = s.strip_prefix('+') {
            s = rest;
        }

        let (base, exponent) = split_exponent(s)?;
        let (int_part, frac_part) = split_decimal_base(base)?;
        if int_part.is_empty() && frac_part.is_empty() {
            return None;
        }
        if !int_part.bytes().all(|b| b.is_ascii_digit())
            || !frac_part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }

        let mut digits = String::with_capacity(int_part.len() + frac_part.len());
        digits.push_str(int_part);
        digits.push_str(frac_part);
        let mut scale = frac_part.len() as i32 - exponent;
        trim_decimal(&mut digits, &mut scale);
        if digits == "0" {
            negative = false;
        }
        Some(Self {
            negative,
            digits,
            scale,
        })
    }

    fn cmp_abs(&self, other: &Self) -> Ordering {
        let left_int = self.digits.len() as i32 - self.scale;
        let right_int = other.digits.len() as i32 - other.scale;
        match left_int.cmp(&right_int) {
            Ordering::Equal => {}
            ord => return ord,
        }

        let scale = self.scale.max(other.scale).max(0) as usize;
        let mut left = self.digits.clone();
        let mut right = other.digits.clone();
        left.extend(std::iter::repeat_n(
            '0',
            scale.saturating_sub(self.scale.max(0) as usize),
        ));
        right.extend(std::iter::repeat_n(
            '0',
            scale.saturating_sub(other.scale.max(0) as usize),
        ));
        left.cmp(&right)
    }
}

impl Ord for ParsedDecimal {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (true, true) => other.cmp_abs(self),
            (false, false) => self.cmp_abs(other),
        }
    }
}

impl PartialOrd for ParsedDecimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn split_exponent(input: &str) -> Option<(&str, i32)> {
    let Some(index) = input.find(['e', 'E']) else {
        return Some((input, 0));
    };
    let exponent = input[index + 1..].parse::<i32>().ok()?;
    Some((&input[..index], exponent))
}

fn split_decimal_base(input: &str) -> Option<(&str, &str)> {
    if let Some(index) = input.find('.') {
        if input[index + 1..].contains('.') {
            return None;
        }
        Some((&input[..index], &input[index + 1..]))
    } else {
        Some((input, ""))
    }
}

fn trim_decimal(digits: &mut String, scale: &mut i32) {
    while digits.len() > 1 && digits.starts_with('0') {
        digits.remove(0);
    }
    while *scale > 0 && digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
        *scale -= 1;
    }
    if digits.is_empty() || digits.bytes().all(|b| b == b'0') {
        digits.clear();
        digits.push('0');
        *scale = 0;
    }
    if *scale < 0 {
        digits.extend(std::iter::repeat_n('0', (-*scale) as usize));
        *scale = 0;
    }
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
            Value::Decimal(0),
            Value::DecimalText("0".to_string()),
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
            (Value::Decimal(10000), Value::Decimal(20000), Ordering::Less),
            (
                Value::DecimalText("3.14".to_string()),
                Value::DecimalText("3.1400".to_string()),
                Ordering::Equal,
            ),
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
            (
                Value::DecimalText("18446744073709551616".to_string()),
                Value::UnsignedInteger(u64::MAX),
                Ordering::Greater,
            ),
            (
                Value::DecimalText("3.14159265358979323846".to_string()),
                Value::DecimalText("3.14159265358979323847".to_string()),
                Ordering::Less,
            ),
            (
                Value::DecimalText("-0.00000000000000000001".to_string()),
                Value::Integer(0),
                Ordering::Less,
            ),
            (
                Value::DecimalText("1e30".to_string()),
                Value::DecimalText("999999999999999999999999999999".to_string()),
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
