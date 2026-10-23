use reddb_wire::query_with_params::{decode_value, encode_value, ParamValue};

#[test]
fn vector_encoding_retains_prefix_and_ieee754_bits_at_an_unaligned_offset() {
    let bits = [
        0,
        0x8000_0000,
        0x3f80_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0042,
        0x7fa0_0123,
    ];
    let value = ParamValue::Vector(bits.map(f32::from_bits).to_vec());
    let mut bytes = vec![0xaa, 0xbb, 0xcc];
    encode_value(&value, &mut bytes).expect("vector encoding");
    assert_eq!(&bytes[..8], &[0xaa, 0xbb, 0xcc, 6, 7, 0, 0, 0]);
    for (encoded, expected) in bytes[8..].chunks_exact(4).zip(bits) {
        assert_eq!(encoded, expected.to_le_bytes());
    }
    let mut position = 3;
    let ParamValue::Vector(decoded) = decode_value(&bytes, &mut position).expect("vector decoding")
    else {
        panic!("vector value");
    };
    assert_eq!(
        decoded.into_iter().map(f32::to_bits).collect::<Vec<_>>(),
        bits
    );
    assert_eq!(position, bytes.len());
}
