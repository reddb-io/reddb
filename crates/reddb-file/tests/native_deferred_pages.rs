use reddb_file::{decode_native_paged_deferred_pages, encode_native_paged_deferred_pages};

#[test]
fn legacy_metadata_and_truncated_reclamation_ledgers_fail_safely() {
    let mut position = 0;
    assert!(decode_native_paged_deferred_pages(&[0; 16], &mut position)
        .unwrap()
        .is_empty());
    assert_eq!(position, 0);
    let mut bytes = Vec::new();
    encode_native_paged_deferred_pages(&mut bytes, &[67, 100, 500]).unwrap();
    assert_eq!(
        decode_native_paged_deferred_pages(&bytes, &mut 0).unwrap(),
        [67, 100, 500]
    );
    for length in 4..bytes.len() {
        assert!(decode_native_paged_deferred_pages(&bytes[..length], &mut 0).is_err());
    }
    let mut duplicate = bytes.clone();
    duplicate[12..16].copy_from_slice(&67_u32.to_le_bytes());
    assert!(decode_native_paged_deferred_pages(&duplicate, &mut 0).is_err());
    assert!(encode_native_paged_deferred_pages(&mut Vec::new(), &[0]).is_err());
    assert!(encode_native_paged_deferred_pages(&mut Vec::new(), &[100, 67]).is_err());
}
