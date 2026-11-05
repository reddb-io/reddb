use reddb_engine::storage::error::StoreError;
use reddb_engine::storage::ordered_secondary_key::encode_ordered_key;
use reddb_types::{value_to_canonical_key, CanonicalKey, CanonicalKeyFamily, Value};

fn encoded(key: &CanonicalKey) -> Vec<u8> {
    let mut buffer = [0; 1016];
    let length = encode_ordered_key(key, &mut buffer).expect("bounded key");
    buffer[..length].to_vec()
}

#[test]
fn ordered_secondary_encoding_matches_canonical_order_and_is_prefix_free() {
    let mut values = vec![
        Value::Null,
        Value::Boolean(false),
        Value::Boolean(true),
        Value::Integer(i64::MIN),
        Value::Integer(-1),
        Value::Integer(0),
        Value::Integer(1),
        Value::Integer(i64::MAX),
        Value::BigInt(-1),
        Value::UnsignedInteger(0),
        Value::UnsignedInteger(u64::MAX),
        Value::Float(-f64::MAX),
        Value::Float(-1.0),
        Value::Float(-0.0),
        Value::Float(0.0),
        Value::Float(f64::MIN_POSITIVE),
        Value::Float(f64::MAX),
        Value::Timestamp(-1),
        Value::Duration(-1),
        Value::Date(i32::MIN),
        Value::Decimal(i64::MIN),
        Value::TimestampMs(-1),
        Value::Latitude(-1),
        Value::Longitude(-1),
        Value::GeoPoint(-1, 1),
        Value::GeoPoint(0, -1),
        Value::Phone(u64::MAX),
        Value::Semver(u32::MAX),
        Value::Time(u32::MAX),
        Value::EnumValue(u8::MAX),
        Value::Port(u16::MAX),
        Value::PageRef(u32::MAX),
        Value::Cidr(0, 32),
        Value::Cidr(u32::MAX, 0),
        Value::Subnet(0, u32::MAX),
        Value::Subnet(1, 0),
    ];
    for text in ["", "\0", "\0\0", "a", "a\0", "a\0b", "ab", "é", "😀"] {
        values.extend([
            Value::text(text),
            Value::Email(text.into()),
            Value::Url(text.into()),
            Value::NodeRef(text.into()),
            Value::EdgeRef(text.into()),
            Value::TableRef(text.into()),
            Value::Password(text.into()),
            Value::VectorRef(text.into(), 0),
            Value::VectorRef(text.into(), u64::MAX),
            Value::RowRef(text.into(), 1),
            Value::DocRef(text.into(), 1),
            Value::KeyRef(text.into(), "".into()),
            Value::KeyRef(text.into(), "\0".into()),
        ]);
    }
    for bytes in [
        vec![],
        vec![0],
        vec![0, 0],
        vec![0, 255],
        vec![1],
        vec![255],
    ] {
        values.extend([Value::Blob(bytes.clone()), Value::Json(bytes)]);
    }
    let keys: Vec<_> = values
        .iter()
        .map(|value| value_to_canonical_key(value).expect("canonical"))
        .collect();
    let bytes: Vec<_> = keys.iter().map(encoded).collect();
    for (left, a) in keys.iter().zip(&bytes) {
        for (right, b) in keys.iter().zip(&bytes) {
            assert_eq!(left.cmp(right), a.cmp(b), "{left:?} / {right:?}");
            if left != right {
                assert!(
                    !a.starts_with(b),
                    "keys must not interleave with appended physical IDs"
                );
            }
        }
    }
}

#[test]
fn ordered_secondary_encoding_numeric_boundaries_and_random_bits() {
    let mut state = 0x2cf3_941a_028e_e19du64;
    let mut keys = Vec::new();
    for _ in 0..4000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        keys.push(CanonicalKey::Signed(
            CanonicalKeyFamily::Integer,
            i64::from_ne_bytes(state.to_ne_bytes()),
        ));
        keys.push(CanonicalKey::Unsigned(
            CanonicalKeyFamily::UnsignedInteger,
            state,
        ));
        // The wire order follows total_cmp even for raw NaN bit patterns.
        keys.push(CanonicalKey::Float(state));
    }
    keys.sort();
    let mut actual: Vec<_> = keys.iter().map(|key| (encoded(key), key.clone())).collect();
    actual.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        actual.into_iter().map(|(_, key)| key).collect::<Vec<_>>(),
        keys
    );
}

#[test]
fn ordered_secondary_encoding_checks_capacity_and_family_before_writing() {
    let key = CanonicalKey::Text(CanonicalKeyFamily::Text, "a\0b".into());
    let expected = encoded(&key);
    for capacity in 0..expected.len() {
        let mut output = vec![0xa5; capacity];
        assert!(
            matches!(encode_ordered_key(&key, &mut output), Err(StoreError::ReadLimitExceeded { required_bytes, limit_bytes }) if required_bytes == expected.len() && limit_bytes == capacity)
        );
        assert!(output.iter().all(|byte| *byte == 0xa5));
    }
    let mut output = vec![0; expected.len()];
    assert_eq!(
        encode_ordered_key(&key, &mut output).expect("exact capacity"),
        expected.len()
    );
    assert_eq!(output, expected);
    let mut output = [0xa5; 32];
    assert!(encode_ordered_key(
        &CanonicalKey::Signed(CanonicalKeyFamily::Text, 0),
        &mut output
    )
    .is_err());
    assert_eq!(output, [0xa5; 32]);
}

#[test]
fn ordered_secondary_encoding_composite_keys_preserve_tuple_order() {
    let keys: Vec<_> = ["", "a", "a\0", "ab"]
        .into_iter()
        .flat_map(|text| {
            [i64::MIN, -1, 0, i64::MAX].into_iter().map(move |number| {
                vec![
                    CanonicalKey::Text(CanonicalKeyFamily::Text, text.into()),
                    CanonicalKey::Signed(CanonicalKeyFamily::Integer, number),
                ]
            })
        })
        .collect();
    let bytes: Vec<Vec<u8>> = keys
        .iter()
        .map(|tuple| tuple.iter().flat_map(encoded).collect())
        .collect();
    for (left, a) in keys.iter().zip(&bytes) {
        for (right, b) in keys.iter().zip(&bytes) {
            assert_eq!(left.cmp(right), a.cmp(b));
        }
    }
}
