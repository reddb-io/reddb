use std::cmp::Ordering;
use std::net::IpAddr;

use super::Value;

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
    Text(CanonicalKeyFamily, String),
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
            Self::Text(CanonicalKeyFamily::NodeRef, v) => Value::NodeRef(v),
            Self::Text(CanonicalKeyFamily::EdgeRef, v) => Value::EdgeRef(v),
            Self::Text(CanonicalKeyFamily::Email, v) => Value::Email(v),
            Self::Text(CanonicalKeyFamily::Url, v) => Value::Url(v),
            Self::Text(CanonicalKeyFamily::TableRef, v) => Value::TableRef(v),
            Self::Text(CanonicalKeyFamily::Password, v) => Value::Password(v),
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
        Value::Text(v) => Some(CanonicalKey::Text(CanonicalKeyFamily::Text, v.to_string())),
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
        Value::NodeRef(v) => Some(CanonicalKey::Text(CanonicalKeyFamily::NodeRef, v.clone())),
        Value::EdgeRef(v) => Some(CanonicalKey::Text(CanonicalKeyFamily::EdgeRef, v.clone())),
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
        Value::Email(v) => Some(CanonicalKey::Text(CanonicalKeyFamily::Email, v.clone())),
        Value::Url(v) => Some(CanonicalKey::Text(CanonicalKeyFamily::Url, v.clone())),
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
        Value::AssetCode(v) => Some(CanonicalKey::Text(CanonicalKeyFamily::Text, v.clone())),
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
        Value::TableRef(v) => Some(CanonicalKey::Text(CanonicalKeyFamily::TableRef, v.clone())),
        Value::PageRef(v) => Some(CanonicalKey::Unsigned(
            CanonicalKeyFamily::PageRef,
            *v as u64,
        )),
        Value::Secret(_) => None,
        Value::Password(v) => Some(CanonicalKey::Text(CanonicalKeyFamily::Password, v.clone())),
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
}
