use crate::storage::schema::Value;
use std::cmp::Ordering;

fn value_type_tag(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Boolean(_) => 1,
        Value::Integer(_) => 2,
        Value::UnsignedInteger(_) => 3,
        Value::Float(_) => 4,
        Value::Text(_) => 5,
        Value::Blob(_) => 6,
        Value::Timestamp(_) => 7,
        Value::Duration(_) => 8,
        Value::IpAddr(_) => 9,
        Value::MacAddr(_) => 10,
        Value::Vector(_) => 11,
        Value::Json(_) => 12,
        Value::Uuid(_) => 13,
        Value::NodeRef(_) => 14,
        Value::EdgeRef(_) => 15,
        Value::VectorRef(_, _) => 16,
        Value::RowRef(_, _) => 17,
        Value::Color(_) => 18,
        Value::Email(_) => 19,
        Value::Url(_) => 20,
        Value::Phone(_) => 21,
        Value::Semver(_) => 22,
        Value::Cidr(_, _) => 23,
        Value::Date(_) => 24,
        Value::Time(_) => 25,
        Value::Decimal(_) => 26,
        Value::EnumValue(_) => 27,
        Value::Array(_) => 28,
        Value::TimestampMs(_) => 29,
        Value::Ipv4(_) => 30,
        Value::Ipv6(_) => 31,
        Value::Subnet(_, _) => 32,
        Value::Port(_) => 33,
        Value::Latitude(_) => 34,
        Value::Longitude(_) => 35,
        Value::GeoPoint(_, _) => 36,
        Value::Country2(_) => 37,
        Value::Country3(_) => 38,
        Value::Lang2(_) => 39,
        Value::Lang5(_) => 40,
        Value::Currency(_) => 41,
        Value::ColorAlpha(_) => 42,
        Value::BigInt(_) => 43,
        Value::KeyRef(_, _) => 44,
        Value::DocRef(_, _) => 45,
        Value::TableRef(_) => 46,
        Value::PageRef(_) => 47,
        Value::Secret(_) => 48,
        Value::Password(_) => 49,
    }
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
