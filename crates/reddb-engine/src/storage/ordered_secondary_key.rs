//! Version-one ordered projection for native secondary trees.
//!
//! Explicit family tags follow CanonicalKey's order. Signed numbers flip their
//! sign bit; floats use the reversible total_cmp transform. Variable fields
//! escape zero as 00 ff and terminate with 00 00. Thus complete keys are prefix
//! free, including when physical IDs are appended or fixed-arity tuples are
//! formed by concatenation. No record payload, heap buffer or network is needed:
//! encoding costs O(key bytes), with output bounded by the caller's scratch.
use super::error::StoreError;
use reddb_types::{CanonicalKey, CanonicalKeyFamily};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Null,
    Boolean,
    Signed,
    Unsigned,
    Float,
    Text,
    Bytes,
    TextU64,
    TextText,
    U32U8,
    U32U32,
    I32I32,
}

// These tags are a durable codec contract, never enum-discriminant casts.
fn family_encoding(family: CanonicalKeyFamily) -> (u8, Shape) {
    use CanonicalKeyFamily as F;
    match family {
        F::Null => (0, Shape::Null),
        F::Boolean => (1, Shape::Boolean),
        F::Integer => (2, Shape::Signed),
        F::BigInt => (3, Shape::Signed),
        F::UnsignedInteger => (4, Shape::Unsigned),
        F::Float => (5, Shape::Float),
        F::Text => (6, Shape::Text),
        F::Blob => (7, Shape::Bytes),
        F::Timestamp => (8, Shape::Signed),
        F::Duration => (9, Shape::Signed),
        F::IpAddr => (10, Shape::Bytes),
        F::MacAddr => (11, Shape::Bytes),
        F::Json => (12, Shape::Bytes),
        F::Uuid => (13, Shape::Bytes),
        F::NodeRef => (14, Shape::Text),
        F::EdgeRef => (15, Shape::Text),
        F::VectorRef => (16, Shape::TextU64),
        F::RowRef => (17, Shape::TextU64),
        F::Color => (18, Shape::Bytes),
        F::Email => (19, Shape::Text),
        F::Url => (20, Shape::Text),
        F::Phone => (21, Shape::Unsigned),
        F::Semver => (22, Shape::Unsigned),
        F::Cidr => (23, Shape::U32U8),
        F::Date => (24, Shape::Signed),
        F::Time => (25, Shape::Unsigned),
        F::Decimal => (26, Shape::Signed),
        F::EnumValue => (27, Shape::Unsigned),
        F::TimestampMs => (28, Shape::Signed),
        F::Ipv4 => (29, Shape::Bytes),
        F::Ipv6 => (30, Shape::Bytes),
        F::Subnet => (31, Shape::U32U32),
        F::Port => (32, Shape::Unsigned),
        F::Latitude => (33, Shape::Signed),
        F::Longitude => (34, Shape::Signed),
        F::GeoPoint => (35, Shape::I32I32),
        F::Country2 => (36, Shape::Bytes),
        F::Country3 => (37, Shape::Bytes),
        F::Lang2 => (38, Shape::Bytes),
        F::Lang5 => (39, Shape::Bytes),
        F::Currency => (40, Shape::Bytes),
        F::ColorAlpha => (41, Shape::Bytes),
        F::KeyRef => (42, Shape::TextText),
        F::DocRef => (43, Shape::TextU64),
        F::TableRef => (44, Shape::Text),
        F::PageRef => (45, Shape::Unsigned),
        F::Password => (46, Shape::Text),
    }
}

fn escaped_length(bytes: &[u8]) -> usize {
    bytes
        .len()
        .saturating_add(bytes.iter().filter(|byte| **byte == 0).count())
        .saturating_add(2)
}

fn payload_shape_length(key: &CanonicalKey) -> (Shape, usize) {
    use CanonicalKey as K;
    match key {
        K::Null => (Shape::Null, 0),
        K::Boolean(_) => (Shape::Boolean, 1),
        K::Signed(..) => (Shape::Signed, 8),
        K::Unsigned(..) => (Shape::Unsigned, 8),
        K::Float(_) => (Shape::Float, 8),
        K::Text(_, text) => (Shape::Text, escaped_length(text.as_bytes())),
        K::Bytes(_, bytes) => (Shape::Bytes, escaped_length(bytes)),
        K::PairTextU64(_, text, _) => (
            Shape::TextU64,
            escaped_length(text.as_bytes()).saturating_add(8),
        ),
        K::PairTextText(_, left, right) => (
            Shape::TextText,
            escaped_length(left.as_bytes()).saturating_add(escaped_length(right.as_bytes())),
        ),
        K::PairU32U8(..) => (Shape::U32U8, 5),
        K::PairU32U32(..) => (Shape::U32U32, 8),
        K::PairI32I32(..) => (Shape::I32I32, 8),
    }
}

struct Writer<'a> {
    output: &'a mut [u8],
    length: usize,
}

impl Writer<'_> {
    fn bytes(&mut self, bytes: &[u8]) {
        let end = self.length + bytes.len();
        self.output[self.length..end].copy_from_slice(bytes);
        self.length = end;
    }

    fn escaped(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.bytes(&[*byte]);
            if *byte == 0 {
                self.bytes(&[255]);
            }
        }
        self.bytes(&[0, 0]);
    }
}

/// Encode without allocating. Capacity and family errors leave output intact.
/// Concatenate encodings only for tuples of fixed arity in the same index.
/// Persisted projection identities must include this codec's version (1).
pub fn encode_ordered_key(key: &CanonicalKey, output: &mut [u8]) -> Result<usize, StoreError> {
    let (tag, expected_shape) = family_encoding(key.family());
    let (shape, payload_length) = payload_shape_length(key);
    if shape != expected_shape {
        return Err(StoreError::Serialization(
            "ordered key family/value mismatch".into(),
        ));
    }
    let required_bytes = payload_length.saturating_add(1);
    if required_bytes > output.len() {
        return Err(StoreError::ReadLimitExceeded {
            required_bytes,
            limit_bytes: output.len(),
        });
    }
    let mut writer = Writer { output, length: 0 };
    writer.bytes(&[tag]);
    use CanonicalKey as K;
    match key {
        K::Null => {}
        K::Boolean(value) => writer.bytes(&[u8::from(*value)]),
        K::Signed(_, value) => {
            writer.bytes(&(u64::from_ne_bytes(value.to_ne_bytes()) ^ (1 << 63)).to_be_bytes())
        }
        K::Unsigned(_, value) => writer.bytes(&value.to_be_bytes()),
        K::Float(bits) => {
            let ordered = if bits & (1 << 63) != 0 {
                !bits
            } else {
                bits ^ (1 << 63)
            };
            writer.bytes(&ordered.to_be_bytes());
        }
        K::Text(_, text) => writer.escaped(text.as_bytes()),
        K::Bytes(_, bytes) => writer.escaped(bytes),
        K::PairTextU64(_, text, value) => {
            writer.escaped(text.as_bytes());
            writer.bytes(&value.to_be_bytes());
        }
        K::PairTextText(_, left, right) => {
            writer.escaped(left.as_bytes());
            writer.escaped(right.as_bytes());
        }
        K::PairU32U8(_, left, right) => {
            writer.bytes(&left.to_be_bytes());
            writer.bytes(&[*right]);
        }
        K::PairU32U32(_, left, right) => {
            writer.bytes(&left.to_be_bytes());
            writer.bytes(&right.to_be_bytes());
        }
        K::PairI32I32(_, left, right) => {
            writer.bytes(&(u32::from_ne_bytes(left.to_ne_bytes()) ^ (1 << 31)).to_be_bytes());
            writer.bytes(&(u32::from_ne_bytes(right.to_ne_bytes()) ^ (1 << 31)).to_be_bytes());
        }
    }
    assert_eq!(
        writer.length, required_bytes,
        "ordered key size matches admitted scratch"
    );
    Ok(writer.length)
}
