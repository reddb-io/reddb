use std::cmp::Ordering;
use std::net::IpAddr;
use std::sync::Arc;

use crate::types::Value;

/// Stable key family for ordered secondary indexes.
///
/// Families are intentionally narrow: range pushdown is only considered safe
/// when all indexed values in a column belong to the same family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CanonicalKeyFamily {
    Null,
    Boolean,
    Integer,
    BigInt,
    UnsignedInteger,
    Float,
    Text,
    Blob,
    Timestamp,
    Duration,
    IpAddr,
    MacAddr,
    Json,
    Uuid,
    NodeRef,
    EdgeRef,
    VectorRef,
    RowRef,
    Color,
    Email,
    Url,
    Phone,
    Semver,
    Cidr,
    Date,
    Time,
    Decimal,
    EnumValue,
    TimestampMs,
    Ipv4,
    Ipv6,
    Subnet,
    Port,
    Latitude,
    Longitude,
    GeoPoint,
    Country2,
    Country3,
    Lang2,
    Lang5,
    Currency,
    ColorAlpha,
    KeyRef,
    DocRef,
    TableRef,
    PageRef,
    Password,
    DecimalText,
}

/// Canonical multi-type key used by ordered in-memory indexes.
///
/// The ordering is stable and type-aware. Different families never compare
/// equal and range pushdown is only enabled when a column stays within one
/// family. Exact point lookups remain safe even when a column has mixed
/// families because BTree point seeks are still exact on the encoded key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CanonicalKey {
    Null,
    Boolean(bool),
    Signed(CanonicalKeyFamily, i64),
    Unsigned(CanonicalKeyFamily, u64),
    Float(u64),
    /// Text-kind values. `Arc<str>` instead of `String` so
    /// `Value::Text(Arc<str>)` roundtrips free (Arc bump) rather than
    /// allocating a new String per encode. Text-like variants built
    /// from `String` (NodeRef, EdgeRef, Email, Url, TableRef,
    /// Password) pay one Arc::from allocation at encode time — same
    /// cost as the previous String clone. Net: GROUP BY over a
    /// `TEXT` column stops paying N allocations per scan.
    Text(CanonicalKeyFamily, Arc<str>),
    Bytes(CanonicalKeyFamily, Vec<u8>),
    PairTextU64(CanonicalKeyFamily, String, u64),
    PairTextText(CanonicalKeyFamily, String, String),
    PairU32U8(CanonicalKeyFamily, u32, u8),
    PairU32U32(CanonicalKeyFamily, u32, u32),
    PairI32I32(CanonicalKeyFamily, i32, i32),
}

impl CanonicalKey {
    pub fn family(&self) -> CanonicalKeyFamily {
        match self {
            Self::Null => CanonicalKeyFamily::Null,
            Self::Boolean(_) => CanonicalKeyFamily::Boolean,
            Self::Signed(family, _) => *family,
            Self::Unsigned(family, _) => *family,
            Self::Float(_) => CanonicalKeyFamily::Float,
            Self::Text(family, _) => *family,
            Self::Bytes(family, _) => *family,
            Self::PairTextU64(family, _, _) => *family,
            Self::PairTextText(family, _, _) => *family,
            Self::PairU32U8(family, _, _) => *family,
            Self::PairU32U32(family, _, _) => *family,
            Self::PairI32I32(family, _, _) => *family,
        }
    }

    pub fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Boolean(v) => Value::Boolean(v),
            Self::Signed(CanonicalKeyFamily::Integer, v) => Value::Integer(v),
            Self::Signed(CanonicalKeyFamily::BigInt, v) => Value::BigInt(v),
            Self::Signed(CanonicalKeyFamily::Timestamp, v) => Value::Timestamp(v),
            Self::Signed(CanonicalKeyFamily::Duration, v) => Value::Duration(v),
            Self::Signed(CanonicalKeyFamily::Date, v) => Value::Date(v as i32),
            Self::Signed(CanonicalKeyFamily::Decimal, v) => Value::Decimal(v),
            Self::Signed(CanonicalKeyFamily::TimestampMs, v) => Value::TimestampMs(v),
            Self::Signed(CanonicalKeyFamily::Latitude, v) => Value::Latitude(v as i32),
            Self::Signed(CanonicalKeyFamily::Longitude, v) => Value::Longitude(v as i32),
            Self::Unsigned(CanonicalKeyFamily::UnsignedInteger, v) => Value::UnsignedInteger(v),
            Self::Unsigned(CanonicalKeyFamily::Phone, v) => Value::Phone(v),
            Self::Unsigned(CanonicalKeyFamily::Semver, v) => Value::Semver(v as u32),
            Self::Unsigned(CanonicalKeyFamily::Time, v) => Value::Time(v as u32),
            Self::Unsigned(CanonicalKeyFamily::EnumValue, v) => Value::EnumValue(v as u8),
            Self::Unsigned(CanonicalKeyFamily::Port, v) => Value::Port(v as u16),
            Self::Unsigned(CanonicalKeyFamily::PageRef, v) => Value::PageRef(v as u32),
            Self::Float(bits) => Value::Float(f64::from_bits(bits)),
            Self::Text(CanonicalKeyFamily::Text, v) => Value::text(v),
            Self::Text(CanonicalKeyFamily::NodeRef, v) => Value::NodeRef(v.to_string()),
            Self::Text(CanonicalKeyFamily::EdgeRef, v) => Value::EdgeRef(v.to_string()),
            Self::Text(CanonicalKeyFamily::Email, v) => Value::Email(v.to_string()),
            Self::Text(CanonicalKeyFamily::Url, v) => Value::Url(v.to_string()),
            Self::Text(CanonicalKeyFamily::TableRef, v) => Value::TableRef(v.to_string()),
            Self::Text(CanonicalKeyFamily::Password, v) => Value::Password(v.to_string()),
            Self::Text(CanonicalKeyFamily::DecimalText, v) => Value::DecimalText(v.to_string()),
            Self::Bytes(CanonicalKeyFamily::Blob, v) => Value::Blob(v),
            Self::Bytes(CanonicalKeyFamily::MacAddr, v) => {
                let mut out = [0u8; 6];
                out.copy_from_slice(&v[..6]);
                Value::MacAddr(out)
            }
            Self::Bytes(CanonicalKeyFamily::Json, v) => Value::Json(v),
            Self::Bytes(CanonicalKeyFamily::Uuid, v) => {
                let mut out = [0u8; 16];
                out.copy_from_slice(&v[..16]);
                Value::Uuid(out)
            }
            Self::Bytes(CanonicalKeyFamily::IpAddr, v) => {
                let mut out = [0u8; 16];
                out.copy_from_slice(&v[..16]);
                Value::IpAddr(IpAddr::from(out))
            }
            Self::Bytes(CanonicalKeyFamily::Ipv4, v) => {
                let mut out = [0u8; 4];
                out.copy_from_slice(&v[..4]);
                Value::Ipv4(u32::from_be_bytes(out))
            }
            Self::Bytes(CanonicalKeyFamily::Ipv6, v) => {
                let mut out = [0u8; 16];
                out.copy_from_slice(&v[..16]);
                Value::Ipv6(out)
            }
            Self::Bytes(CanonicalKeyFamily::Color, v) => {
                let mut out = [0u8; 3];
                out.copy_from_slice(&v[..3]);
                Value::Color(out)
            }
            Self::Bytes(CanonicalKeyFamily::Country2, v) => {
                let mut out = [0u8; 2];
                out.copy_from_slice(&v[..2]);
                Value::Country2(out)
            }
            Self::Bytes(CanonicalKeyFamily::Country3, v) => {
                let mut out = [0u8; 3];
                out.copy_from_slice(&v[..3]);
                Value::Country3(out)
            }
            Self::Bytes(CanonicalKeyFamily::Lang2, v) => {
                let mut out = [0u8; 2];
                out.copy_from_slice(&v[..2]);
                Value::Lang2(out)
            }
            Self::Bytes(CanonicalKeyFamily::Lang5, v) => {
                let mut out = [0u8; 5];
                out.copy_from_slice(&v[..5]);
                Value::Lang5(out)
            }
            Self::Bytes(CanonicalKeyFamily::Currency, v) => {
                let mut out = [0u8; 3];
                out.copy_from_slice(&v[..3]);
                Value::Currency(out)
            }
            Self::Bytes(CanonicalKeyFamily::ColorAlpha, v) => {
                let mut out = [0u8; 4];
                out.copy_from_slice(&v[..4]);
                Value::ColorAlpha(out)
            }
            Self::PairTextU64(CanonicalKeyFamily::VectorRef, collection, id) => {
                Value::VectorRef(collection, id)
            }
            Self::PairTextU64(CanonicalKeyFamily::RowRef, collection, id) => {
                Value::RowRef(collection, id)
            }
            Self::PairTextU64(CanonicalKeyFamily::DocRef, collection, id) => {
                Value::DocRef(collection, id)
            }
            Self::PairTextText(CanonicalKeyFamily::KeyRef, collection, key) => {
                Value::KeyRef(collection, key)
            }
            Self::PairU32U8(CanonicalKeyFamily::Cidr, ip, prefix) => Value::Cidr(ip, prefix),
            Self::PairU32U32(CanonicalKeyFamily::Subnet, ip, mask) => Value::Subnet(ip, mask),
            Self::PairI32I32(CanonicalKeyFamily::GeoPoint, lat, lon) => Value::GeoPoint(lat, lon),
            _ => unreachable!("canonical key family/value mismatch"),
        }
    }
}

impl Ord for CanonicalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let family_cmp = self.family().cmp(&other.family());
        if family_cmp != Ordering::Equal {
            return family_cmp;
        }
        match (self, other) {
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Boolean(left), Self::Boolean(right)) => left.cmp(right),
            (Self::Signed(_, left), Self::Signed(_, right)) => left.cmp(right),
            (Self::Unsigned(_, left), Self::Unsigned(_, right)) => left.cmp(right),
            (Self::Float(left), Self::Float(right)) => {
                f64::from_bits(*left).total_cmp(&f64::from_bits(*right))
            }
            (Self::Text(_, left), Self::Text(_, right)) => left.cmp(right),
            (Self::Bytes(_, left), Self::Bytes(_, right)) => left.cmp(right),
            (Self::PairTextU64(_, l_text, l_num), Self::PairTextU64(_, r_text, r_num)) => {
                l_text.cmp(r_text).then_with(|| l_num.cmp(r_num))
            }
            (Self::PairTextText(_, l1, l2), Self::PairTextText(_, r1, r2)) => {
                l1.cmp(r1).then_with(|| l2.cmp(r2))
            }
            (Self::PairU32U8(_, l1, l2), Self::PairU32U8(_, r1, r2)) => {
                l1.cmp(r1).then_with(|| l2.cmp(r2))
            }
            (Self::PairU32U32(_, l1, l2), Self::PairU32U32(_, r1, r2)) => {
                l1.cmp(r1).then_with(|| l2.cmp(r2))
            }
            (Self::PairI32I32(_, l1, l2), Self::PairI32I32(_, r1, r2)) => {
                l1.cmp(r1).then_with(|| l2.cmp(r2))
            }
            _ => Ordering::Equal,
        }
    }
}

impl PartialOrd for CanonicalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub fn value_to_canonical_key(value: &Value) -> Option<CanonicalKey> {
    match value {
        Value::Null => Some(CanonicalKey::Null),
        Value::Integer(v) => Some(CanonicalKey::Signed(CanonicalKeyFamily::Integer, *v)),
        Value::UnsignedInteger(v) => Some(CanonicalKey::Unsigned(
            CanonicalKeyFamily::UnsignedInteger,
            *v,
        )),
        Value::Float(v) if v.is_finite() => Some(CanonicalKey::Float(v.to_bits())),
        Value::Float(_) => None,
        Value::Text(v) => Some(CanonicalKey::Text(CanonicalKeyFamily::Text, v.clone())),
        Value::Blob(v) => Some(CanonicalKey::Bytes(CanonicalKeyFamily::Blob, v.clone())),
        Value::Boolean(v) => Some(CanonicalKey::Boolean(*v)),
        Value::Timestamp(v) => Some(CanonicalKey::Signed(CanonicalKeyFamily::Timestamp, *v)),
        Value::Duration(v) => Some(CanonicalKey::Signed(CanonicalKeyFamily::Duration, *v)),
        Value::IpAddr(v) => Some(CanonicalKey::Bytes(
            CanonicalKeyFamily::IpAddr,
            ipaddr_to_bytes(*v).to_vec(),
        )),
        Value::MacAddr(v) => Some(CanonicalKey::Bytes(CanonicalKeyFamily::MacAddr, v.to_vec())),
        Value::Vector(_) => None,
        Value::Json(v) => Some(CanonicalKey::Bytes(CanonicalKeyFamily::Json, v.clone())),
        Value::Uuid(v) => Some(CanonicalKey::Bytes(CanonicalKeyFamily::Uuid, v.to_vec())),
        Value::NodeRef(v) => Some(CanonicalKey::Text(
            CanonicalKeyFamily::NodeRef,
            Arc::from(v.as_str()),
        )),
        Value::EdgeRef(v) => Some(CanonicalKey::Text(
            CanonicalKeyFamily::EdgeRef,
            Arc::from(v.as_str()),
        )),
        Value::VectorRef(collection, id) => Some(CanonicalKey::PairTextU64(
            CanonicalKeyFamily::VectorRef,
            collection.clone(),
            *id,
        )),
        Value::RowRef(collection, id) => Some(CanonicalKey::PairTextU64(
            CanonicalKeyFamily::RowRef,
            collection.clone(),
            *id,
        )),
        Value::Color(v) => Some(CanonicalKey::Bytes(CanonicalKeyFamily::Color, v.to_vec())),
        Value::Email(v) => Some(CanonicalKey::Text(
            CanonicalKeyFamily::Email,
            Arc::from(v.as_str()),
        )),
        Value::Url(v) => Some(CanonicalKey::Text(
            CanonicalKeyFamily::Url,
            Arc::from(v.as_str()),
        )),
        Value::Phone(v) => Some(CanonicalKey::Unsigned(CanonicalKeyFamily::Phone, *v)),
        Value::Semver(v) => Some(CanonicalKey::Unsigned(
            CanonicalKeyFamily::Semver,
            *v as u64,
        )),
        Value::Cidr(ip, prefix) => Some(CanonicalKey::PairU32U8(
            CanonicalKeyFamily::Cidr,
            *ip,
            *prefix,
        )),
        Value::Date(v) => Some(CanonicalKey::Signed(
            CanonicalKeyFamily::Date,
            i64::from(*v),
        )),
        Value::Time(v) => Some(CanonicalKey::Unsigned(CanonicalKeyFamily::Time, *v as u64)),
        Value::Decimal(v) => Some(CanonicalKey::Signed(CanonicalKeyFamily::Decimal, *v)),
        Value::EnumValue(v) => Some(CanonicalKey::Unsigned(
            CanonicalKeyFamily::EnumValue,
            *v as u64,
        )),
        Value::Array(_) => None,
        Value::TimestampMs(v) => Some(CanonicalKey::Signed(CanonicalKeyFamily::TimestampMs, *v)),
        Value::Ipv4(v) => Some(CanonicalKey::Bytes(
            CanonicalKeyFamily::Ipv4,
            v.to_be_bytes().to_vec(),
        )),
        Value::Ipv6(v) => Some(CanonicalKey::Bytes(CanonicalKeyFamily::Ipv6, v.to_vec())),
        Value::Subnet(ip, mask) => Some(CanonicalKey::PairU32U32(
            CanonicalKeyFamily::Subnet,
            *ip,
            *mask,
        )),
        Value::Port(v) => Some(CanonicalKey::Unsigned(CanonicalKeyFamily::Port, *v as u64)),
        Value::Latitude(v) => Some(CanonicalKey::Signed(
            CanonicalKeyFamily::Latitude,
            i64::from(*v),
        )),
        Value::Longitude(v) => Some(CanonicalKey::Signed(
            CanonicalKeyFamily::Longitude,
            i64::from(*v),
        )),
        Value::GeoPoint(lat, lon) => Some(CanonicalKey::PairI32I32(
            CanonicalKeyFamily::GeoPoint,
            *lat,
            *lon,
        )),
        Value::Country2(v) => Some(CanonicalKey::Bytes(
            CanonicalKeyFamily::Country2,
            v.to_vec(),
        )),
        Value::Country3(v) => Some(CanonicalKey::Bytes(
            CanonicalKeyFamily::Country3,
            v.to_vec(),
        )),
        Value::Lang2(v) => Some(CanonicalKey::Bytes(CanonicalKeyFamily::Lang2, v.to_vec())),
        Value::Lang5(v) => Some(CanonicalKey::Bytes(CanonicalKeyFamily::Lang5, v.to_vec())),
        Value::Currency(v) => Some(CanonicalKey::Bytes(
            CanonicalKeyFamily::Currency,
            v.to_vec(),
        )),
        Value::AssetCode(v) => Some(CanonicalKey::Text(
            CanonicalKeyFamily::Text,
            Arc::from(v.as_str()),
        )),
        Value::Money { .. } => None,
        Value::ColorAlpha(v) => Some(CanonicalKey::Bytes(
            CanonicalKeyFamily::ColorAlpha,
            v.to_vec(),
        )),
        Value::BigInt(v) => Some(CanonicalKey::Signed(CanonicalKeyFamily::BigInt, *v)),
        Value::KeyRef(collection, key) => Some(CanonicalKey::PairTextText(
            CanonicalKeyFamily::KeyRef,
            collection.clone(),
            key.clone(),
        )),
        Value::DocRef(collection, id) => Some(CanonicalKey::PairTextU64(
            CanonicalKeyFamily::DocRef,
            collection.clone(),
            *id,
        )),
        Value::TableRef(v) => Some(CanonicalKey::Text(
            CanonicalKeyFamily::TableRef,
            Arc::from(v.as_str()),
        )),
        Value::PageRef(v) => Some(CanonicalKey::Unsigned(
            CanonicalKeyFamily::PageRef,
            *v as u64,
        )),
        Value::Secret(_) => None,
        Value::Password(v) => Some(CanonicalKey::Text(
            CanonicalKeyFamily::Password,
            Arc::from(v.as_str()),
        )),
        Value::DecimalText(v) => Some(CanonicalKey::Text(
            CanonicalKeyFamily::DecimalText,
            Arc::from(v.as_str()),
        )),
    }
}

fn ipaddr_to_bytes(value: IpAddr) -> [u8; 16] {
    match value {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
        IpAddr::V6(v6) => v6.octets(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn canonical_keys_are_ordered_inside_their_family() {
        let low = value_to_canonical_key(&Value::Integer(-5)).unwrap();
        let high = value_to_canonical_key(&Value::Integer(20)).unwrap();
        assert!(low < high);
    }

    #[test]
    fn float_keys_reject_nan() {
        assert!(value_to_canonical_key(&Value::Float(f64::NAN)).is_none());
    }

    #[test]
    fn text_and_email_use_different_families() {
        let text = value_to_canonical_key(&Value::text("alice".to_string())).unwrap();
        let email = value_to_canonical_key(&Value::Email("alice@example.com".to_string())).unwrap();
        assert_ne!(text.family(), email.family());
    }

    #[test]
    fn value_to_canonical_key_covers_supported_families() {
        let samples = [
            (Value::Null, CanonicalKeyFamily::Null),
            (Value::Boolean(true), CanonicalKeyFamily::Boolean),
            (Value::Integer(-7), CanonicalKeyFamily::Integer),
            (Value::BigInt(-9), CanonicalKeyFamily::BigInt),
            (
                Value::UnsignedInteger(7),
                CanonicalKeyFamily::UnsignedInteger,
            ),
            (Value::Float(1.5), CanonicalKeyFamily::Float),
            (Value::text("text"), CanonicalKeyFamily::Text),
            (Value::Blob(vec![1, 2]), CanonicalKeyFamily::Blob),
            (Value::Timestamp(10), CanonicalKeyFamily::Timestamp),
            (Value::Duration(11), CanonicalKeyFamily::Duration),
            (
                Value::IpAddr(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
                CanonicalKeyFamily::IpAddr,
            ),
            (
                Value::IpAddr(IpAddr::V6(Ipv6Addr::LOCALHOST)),
                CanonicalKeyFamily::IpAddr,
            ),
            (
                Value::MacAddr([1, 2, 3, 4, 5, 6]),
                CanonicalKeyFamily::MacAddr,
            ),
            (
                Value::Json(br#"{"a":1}"#.to_vec()),
                CanonicalKeyFamily::Json,
            ),
            (Value::Uuid([7; 16]), CanonicalKeyFamily::Uuid),
            (
                Value::NodeRef("n1".to_string()),
                CanonicalKeyFamily::NodeRef,
            ),
            (
                Value::EdgeRef("e1".to_string()),
                CanonicalKeyFamily::EdgeRef,
            ),
            (
                Value::VectorRef("vecs".to_string(), 9),
                CanonicalKeyFamily::VectorRef,
            ),
            (
                Value::RowRef("rows".to_string(), 10),
                CanonicalKeyFamily::RowRef,
            ),
            (Value::Color([0xAA, 0xBB, 0xCC]), CanonicalKeyFamily::Color),
            (
                Value::Email("a@example.com".to_string()),
                CanonicalKeyFamily::Email,
            ),
            (
                Value::Url("https://example.com".to_string()),
                CanonicalKeyFamily::Url,
            ),
            (Value::Phone(5511999), CanonicalKeyFamily::Phone),
            (Value::Semver(1_002_003), CanonicalKeyFamily::Semver),
            (Value::Cidr(10 << 24, 8), CanonicalKeyFamily::Cidr),
            (Value::Date(20_000), CanonicalKeyFamily::Date),
            (Value::Time(43_200_000), CanonicalKeyFamily::Time),
            (Value::Decimal(123_456), CanonicalKeyFamily::Decimal),
            (Value::EnumValue(3), CanonicalKeyFamily::EnumValue),
            (Value::TimestampMs(123_456), CanonicalKeyFamily::TimestampMs),
            (Value::Ipv4(0x7f000001), CanonicalKeyFamily::Ipv4),
            (Value::Ipv6([1; 16]), CanonicalKeyFamily::Ipv6),
            (
                Value::Subnet(10 << 24, 0xff000000),
                CanonicalKeyFamily::Subnet,
            ),
            (Value::Port(5432), CanonicalKeyFamily::Port),
            (Value::Latitude(-23_550_520), CanonicalKeyFamily::Latitude),
            (Value::Longitude(-46_633_308), CanonicalKeyFamily::Longitude),
            (
                Value::GeoPoint(-23_550_520, -46_633_308),
                CanonicalKeyFamily::GeoPoint,
            ),
            (Value::Country2(*b"BR"), CanonicalKeyFamily::Country2),
            (Value::Country3(*b"BRA"), CanonicalKeyFamily::Country3),
            (Value::Lang2(*b"pt"), CanonicalKeyFamily::Lang2),
            (Value::Lang5(*b"pt-BR"), CanonicalKeyFamily::Lang5),
            (Value::Currency(*b"USD"), CanonicalKeyFamily::Currency),
            (
                Value::AssetCode("BTC".to_string()),
                CanonicalKeyFamily::Text,
            ),
            (
                Value::ColorAlpha([0xAA, 0xBB, 0xCC, 0xDD]),
                CanonicalKeyFamily::ColorAlpha,
            ),
            (
                Value::KeyRef("kv".to_string(), "key".to_string()),
                CanonicalKeyFamily::KeyRef,
            ),
            (
                Value::DocRef("docs".to_string(), 42),
                CanonicalKeyFamily::DocRef,
            ),
            (
                Value::TableRef("users".to_string()),
                CanonicalKeyFamily::TableRef,
            ),
            (Value::PageRef(12), CanonicalKeyFamily::PageRef),
            (
                Value::Password("$argon2id$v=19$hash".to_string()),
                CanonicalKeyFamily::Password,
            ),
        ];

        for (value, family) in samples {
            let key = value_to_canonical_key(&value).expect("indexable value");
            assert_eq!(key.family(), family, "{value:?}");
        }
    }

    #[test]
    fn canonical_keys_round_trip_to_values_by_shape() {
        let keys = vec![
            CanonicalKey::Null,
            CanonicalKey::Boolean(false),
            CanonicalKey::Signed(CanonicalKeyFamily::Integer, -1),
            CanonicalKey::Signed(CanonicalKeyFamily::BigInt, -2),
            CanonicalKey::Signed(CanonicalKeyFamily::Timestamp, 3),
            CanonicalKey::Signed(CanonicalKeyFamily::Duration, 4),
            CanonicalKey::Signed(CanonicalKeyFamily::Date, 5),
            CanonicalKey::Signed(CanonicalKeyFamily::Decimal, 6),
            CanonicalKey::Signed(CanonicalKeyFamily::TimestampMs, 7),
            CanonicalKey::Signed(CanonicalKeyFamily::Latitude, 8),
            CanonicalKey::Signed(CanonicalKeyFamily::Longitude, 9),
            CanonicalKey::Unsigned(CanonicalKeyFamily::UnsignedInteger, 10),
            CanonicalKey::Unsigned(CanonicalKeyFamily::Phone, 11),
            CanonicalKey::Unsigned(CanonicalKeyFamily::Semver, 12),
            CanonicalKey::Unsigned(CanonicalKeyFamily::Time, 13),
            CanonicalKey::Unsigned(CanonicalKeyFamily::EnumValue, 14),
            CanonicalKey::Unsigned(CanonicalKeyFamily::Port, 15),
            CanonicalKey::Unsigned(CanonicalKeyFamily::PageRef, 16),
            CanonicalKey::Float(1.25f64.to_bits()),
            CanonicalKey::Text(CanonicalKeyFamily::Text, Arc::from("text")),
            CanonicalKey::Text(CanonicalKeyFamily::NodeRef, Arc::from("node")),
            CanonicalKey::Text(CanonicalKeyFamily::EdgeRef, Arc::from("edge")),
            CanonicalKey::Text(CanonicalKeyFamily::Email, Arc::from("a@example.com")),
            CanonicalKey::Text(CanonicalKeyFamily::Url, Arc::from("https://e.test")),
            CanonicalKey::Text(CanonicalKeyFamily::TableRef, Arc::from("users")),
            CanonicalKey::Text(CanonicalKeyFamily::Password, Arc::from("hash")),
            CanonicalKey::Bytes(CanonicalKeyFamily::Blob, vec![1, 2]),
            CanonicalKey::Bytes(CanonicalKeyFamily::MacAddr, vec![1, 2, 3, 4, 5, 6]),
            CanonicalKey::Bytes(CanonicalKeyFamily::Json, br#"{"ok":true}"#.to_vec()),
            CanonicalKey::Bytes(CanonicalKeyFamily::Uuid, vec![7; 16]),
            CanonicalKey::Bytes(CanonicalKeyFamily::IpAddr, vec![0; 16]),
            CanonicalKey::Bytes(CanonicalKeyFamily::Ipv4, vec![127, 0, 0, 1]),
            CanonicalKey::Bytes(CanonicalKeyFamily::Ipv6, vec![8; 16]),
            CanonicalKey::Bytes(CanonicalKeyFamily::Color, vec![0xAA, 0xBB, 0xCC]),
            CanonicalKey::Bytes(CanonicalKeyFamily::Country2, b"BR".to_vec()),
            CanonicalKey::Bytes(CanonicalKeyFamily::Country3, b"BRA".to_vec()),
            CanonicalKey::Bytes(CanonicalKeyFamily::Lang2, b"pt".to_vec()),
            CanonicalKey::Bytes(CanonicalKeyFamily::Lang5, b"pt-BR".to_vec()),
            CanonicalKey::Bytes(CanonicalKeyFamily::Currency, b"USD".to_vec()),
            CanonicalKey::Bytes(CanonicalKeyFamily::ColorAlpha, vec![1, 2, 3, 4]),
            CanonicalKey::PairTextU64(CanonicalKeyFamily::VectorRef, "v".to_string(), 1),
            CanonicalKey::PairTextU64(CanonicalKeyFamily::RowRef, "r".to_string(), 2),
            CanonicalKey::PairTextU64(CanonicalKeyFamily::DocRef, "d".to_string(), 3),
            CanonicalKey::PairTextText(
                CanonicalKeyFamily::KeyRef,
                "kv".to_string(),
                "key".to_string(),
            ),
            CanonicalKey::PairU32U8(CanonicalKeyFamily::Cidr, 10 << 24, 8),
            CanonicalKey::PairU32U32(CanonicalKeyFamily::Subnet, 10 << 24, 0xff000000),
            CanonicalKey::PairI32I32(CanonicalKeyFamily::GeoPoint, -1, 2),
        ];

        for key in keys {
            let value = key.clone().into_value();
            let recovered = value_to_canonical_key(&value).expect("round-tripped key is indexable");
            assert_eq!(recovered.family(), key.family(), "{key:?}");
        }
    }

    #[test]
    fn non_indexable_values_do_not_produce_canonical_keys() {
        let values = [
            Value::Float(f64::INFINITY),
            Value::Vector(vec![1.0, 2.0]),
            Value::Array(vec![Value::Integer(1)]),
            Value::Money {
                asset_code: "USD".to_string(),
                minor_units: 100,
                scale: 2,
            },
            Value::Secret(vec![1, 2, 3]),
        ];

        for value in values {
            assert!(value_to_canonical_key(&value).is_none(), "{value:?}");
        }
    }

    #[test]
    fn pair_key_ordering_uses_secondary_components() {
        assert!(
            CanonicalKey::PairTextU64(CanonicalKeyFamily::RowRef, "a".to_string(), 1)
                < CanonicalKey::PairTextU64(CanonicalKeyFamily::RowRef, "a".to_string(), 2)
        );
        assert!(
            CanonicalKey::PairTextText(
                CanonicalKeyFamily::KeyRef,
                "a".to_string(),
                "a".to_string()
            ) < CanonicalKey::PairTextText(
                CanonicalKeyFamily::KeyRef,
                "a".to_string(),
                "b".to_string()
            )
        );
        assert!(
            CanonicalKey::PairU32U8(CanonicalKeyFamily::Cidr, 1, 24)
                < CanonicalKey::PairU32U8(CanonicalKeyFamily::Cidr, 2, 8)
        );
        assert!(
            CanonicalKey::PairU32U32(CanonicalKeyFamily::Subnet, 1, 1)
                < CanonicalKey::PairU32U32(CanonicalKeyFamily::Subnet, 1, 2)
        );
        assert!(
            CanonicalKey::PairI32I32(CanonicalKeyFamily::GeoPoint, -1, 0)
                < CanonicalKey::PairI32I32(CanonicalKeyFamily::GeoPoint, 0, -1)
        );
    }
}
