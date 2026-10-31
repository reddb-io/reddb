use reddb_types::{DataType, Value, ValueError};

#[test]
fn compressed_values_require_their_expanded_allocation_before_decode() {
    for value in [
        Value::text("x".repeat(256 * 1024)),
        Value::Blob(vec![7; 256 * 1024]),
    ] {
        let bytes = value.to_bytes();
        assert!(bytes.len() < 1024, "fixture must actually compress");
        assert!(matches!(
            Value::from_bytes_with_limit(&bytes, 4096),
            Err(ValueError::ReadLimitExceeded { .. })
        ));
        let (decoded, consumed) =
            Value::from_bytes_with_limit(&bytes, 1024 * 1024).expect("admitted expanded value");
        assert_eq!(decoded, value);
        assert_eq!(consumed, bytes.len());
    }
}

#[test]
fn arrays_share_one_allowance_and_reserve_their_element_slots() {
    let values = Value::Array(vec![Value::text("a".repeat(4096)); 16]);
    let bytes = values.to_bytes();
    assert!(
        bytes.len() < 8192,
        "encoded input fits; decoded children must share the allowance"
    );
    assert!(matches!(
        Value::from_bytes_with_limit(&bytes, 8192),
        Err(ValueError::ReadLimitExceeded { .. })
    ));
    assert_eq!(
        Value::from_bytes_with_limit(&bytes, 256 * 1024)
            .expect("array")
            .0,
        values
    );
    let nulls = Value::Array(vec![Value::Null; 4096]).to_bytes();
    assert!(nulls.len() < 8192);
    assert!(matches!(
        Value::from_bytes_with_limit(&nulls, 8192),
        Err(ValueError::ReadLimitExceeded { .. })
    ));
}

#[test]
fn impossible_lengths_and_nesting_fail_without_panicking_or_allocating_the_claim() {
    for kind in [
        DataType::Text,
        DataType::Blob,
        DataType::Vector,
        DataType::Array,
        DataType::TextZstd,
    ] {
        let mut bytes = vec![kind.to_byte()];
        bytes.extend_from_slice(&[0xff; 9]);
        bytes.push(1); // u64::MAX
        assert!(
            Value::from_bytes_with_limit(&bytes, 4096).is_err(),
            "{kind:?}"
        );
    }
    let mut bytes = Vec::new();
    for _ in 0..256 {
        bytes.extend_from_slice(&[DataType::Array.to_byte(), 1]);
    }
    bytes.push(0);
    assert!(matches!(
        Value::from_bytes_with_limit(&bytes, 1024 * 1024),
        Err(ValueError::NestingLimitExceeded { .. })
    ));
    let maximum = reddb_types::utils::json::JSON_MAX_NESTING_DEPTH as usize;
    for depth in [maximum, maximum + 1] {
        let mut bytes = Vec::new();
        for _ in 1..depth {
            bytes.extend_from_slice(&[DataType::Array.to_byte(), 1]);
        }
        bytes.extend_from_slice(&[DataType::Array.to_byte(), 0]);
        let result = Value::from_bytes_with_limit(&bytes, 1024 * 1024);
        if depth == maximum {
            assert_eq!(result.expect("exact depth boundary").1, bytes.len());
        } else {
            assert!(matches!(
                result,
                Err(ValueError::NestingLimitExceeded { .. })
            ));
        }
    }
}

#[test]
fn exact_allocation_boundary_is_admitted_and_truncated_payloads_still_fail() {
    let bytes = Value::text("small").to_bytes();
    let first_allowed = (0..4096)
        .find(|limit| Value::from_bytes_with_limit(&bytes, *limit).is_ok())
        .expect("bounded small value");
    assert!(first_allowed > bytes.len());
    assert!(matches!(
        Value::from_bytes_with_limit(&bytes, first_allowed - 1),
        Err(ValueError::ReadLimitExceeded { .. })
    ));
    assert_eq!(
        Value::from_bytes_with_limit(&bytes, first_allowed)
            .expect("exact boundary")
            .0,
        Value::text("small")
    );
    for end in 0..bytes.len() {
        assert!(Value::from_bytes_with_limit(&bytes[..end], 4096).is_err());
    }
}

#[test]
fn bounded_and_legacy_decoders_agree_for_every_registered_uncompressed_tag() {
    for tag in 0..=u8::MAX {
        let Some(kind) = reddb_types::value_codec::type_for_tag(tag) else {
            continue;
        };
        if matches!(kind, DataType::TextZstd | DataType::BlobZstd) {
            continue;
        }
        let mut bytes = vec![0; 65];
        bytes[0] = tag;
        if kind == DataType::IpAddr {
            bytes[1] = 4;
        }
        let expected = Value::from_bytes(&bytes).expect("valid zero/default payload");
        assert_eq!(
            Value::from_bytes_with_limit(&bytes, 4096).expect("bounded default"),
            expected,
            "{kind:?}"
        );
        for end in 0..expected.1 {
            assert!(
                Value::from_bytes_with_limit(&bytes[..end], 4096).is_err(),
                "{kind:?}: {end}"
            );
        }
    }
}

#[test]
fn inconsistent_compressed_length_is_rejected_before_decoding() {
    let mut bytes = Value::text("x".repeat(32 * 1024)).to_bytes();
    assert_eq!(bytes[0], DataType::TextZstd.to_byte());
    // 32768 is a multiple of 128: changing its low varint bit claims an
    // extra output byte without changing any framing or compressed bytes.
    bytes[1] |= 1;
    assert!(matches!(
        Value::from_bytes_with_limit(&bytes, 1024 * 1024),
        Err(ValueError::TruncatedData)
    ));
}

#[test]
fn nonempty_references_vectors_and_nested_values_keep_their_canonical_layout() {
    let value = Value::Array(vec![
        Value::NodeRef("node/é".into()),
        Value::EdgeRef("edge/42".into()),
        Value::VectorRef("vectors".into(), 7),
        Value::RowRef("rows".into(), 9),
        Value::DocRef("docs".into(), 3),
        Value::KeyRef("settings".into(), "theme".into()),
        Value::TableRef("events".into()),
        Value::Money {
            asset_code: "USD".into(),
            minor_units: 12345,
            scale: 2,
        },
        Value::Vector(vec![1.0, 2.0, 3.0]),
        Value::Array(vec![Value::text("nested"), Value::Secret(vec![7; 17])]),
    ]);
    let bytes = value.to_bytes();
    assert_eq!(
        Value::from_bytes_with_limit(&bytes, 64 * 1024).expect("canonical nested value"),
        (value, bytes.len())
    );
    for end in 0..bytes.len() {
        assert!(Value::from_bytes_with_limit(&bytes[..end], 64 * 1024).is_err());
    }
    let mut overflow = vec![DataType::Array.to_byte()];
    overflow.extend_from_slice(&[0x80; 9]);
    overflow.push(2);
    assert!(matches!(
        Value::from_bytes_with_limit(&overflow, 4096),
        Err(ValueError::VarintOverflow)
    ));
}

#[test]
fn frames_without_content_size_must_match_the_declared_expansion_exactly() {
    use std::io::Write;
    let plaintext = vec![b'x'; 1000];
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 3).expect("encoder");
    encoder.include_contentsize(false).expect("omit size hint");
    encoder.write_all(&plaintext).expect("compress");
    let frame = encoder.finish().expect("frame");
    assert_eq!(
        zstd::zstd_safe::get_frame_content_size(&frame).expect("frame header"),
        None
    );
    let encode_length = |mut length: usize, bytes: &mut Vec<u8>| loop {
        let byte = (length & 127) as u8;
        length >>= 7;
        bytes.push(byte | if length > 0 { 128 } else { 0 });
        if length == 0 {
            break;
        }
    };
    for kind in [DataType::TextZstd, DataType::BlobZstd] {
        for declared in [999, 1000, 1001] {
            let mut bytes = vec![kind.to_byte()];
            encode_length(declared, &mut bytes);
            encode_length(frame.len(), &mut bytes);
            bytes.extend_from_slice(&frame);
            let result = Value::from_bytes_with_limit(&bytes, 8192);
            if declared == plaintext.len() {
                let expected = if kind == DataType::TextZstd {
                    Value::text("x".repeat(1000))
                } else {
                    Value::Blob(plaintext.clone())
                };
                assert_eq!(
                    result.expect("valid frame without size hint"),
                    (expected, bytes.len())
                );
            } else {
                assert!(
                    result.is_err(),
                    "{kind:?}: actual 1000, declared {declared}"
                );
            }
        }
    }
}
