//! Embedded recovery tools must reject paged stores before scanning payloads.
use reddb_file::{
    append_native_store_crc32_footer, encode_native_store_header, init_database_header_page,
    salvage_embedded_store, scrub_embedded_store, seal_paged_superblock_slot,
    PAGED_SUPERBLOCK_SLOT_SIZE, PAGED_SUPERBLOCK_ZONE_SIZE,
};

fn paged_with_embedded_looking_payload(copy: usize) -> Vec<u8> {
    let mut bytes = vec![0; PAGED_SUPERBLOCK_ZONE_SIZE];
    let start = copy * PAGED_SUPERBLOCK_SLOT_SIZE;
    let slot = &mut bytes[start..start + PAGED_SUPERBLOCK_SLOT_SIZE];
    init_database_header_page(slot, 4).expect("paged header");
    seal_paged_superblock_slot(slot, copy, 1).expect("sealed header");
    // Carving must not mistake a checksum-valid value for the whole store.
    let mut snapshot = encode_native_store_header(reddb_file::STORE_VERSION_CURRENT);
    snapshot.extend_from_slice(&[0; 16]);
    append_native_store_crc32_footer(&mut snapshot);
    bytes.extend_from_slice(&snapshot);
    bytes
}

#[test]
fn embedded_scrub_rejects_either_paged_authority_without_modifying_source() {
    for copy in 0..2 {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("paged.rdb");
        let bytes = paged_with_embedded_looking_payload(copy);
        std::fs::write(&path, &bytes).expect("fixture");
        let error = scrub_embedded_store(&path, 0, usize::MAX)
            .expect_err("embedded SCRUB cannot verify paged storage");
        assert!(error.to_string().contains("paged"), "{error}");
        assert_eq!(std::fs::read(&path).expect("source"), bytes);
    }
}

#[test]
fn embedded_salvage_does_not_carve_embedded_looking_values_from_paged_storage() {
    for copy in 0..2 {
        let dir = tempfile::tempdir().expect("temporary directory");
        let source = dir.path().join("paged.rdb");
        let destination = dir.path().join("recovered.rdb");
        let bytes = paged_with_embedded_looking_payload(copy);
        std::fs::write(&source, &bytes).expect("fixture");
        let error = salvage_embedded_store(&source, &destination)
            .expect_err("paged store must not be treated as an embedded snapshot");
        assert!(error.to_string().contains("paged"), "{error}");
        assert!(
            !destination.exists(),
            "unsupported recovery created a destination"
        );
        assert_eq!(std::fs::read(&source).expect("source"), bytes);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn large_paged_store_is_rejected_before_whole_file_carving() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let source = dir.path().join("large.rdb");
    let destination = dir.path().join("recovered.rdb");
    std::fs::write(&source, paged_with_embedded_looking_payload(1)).expect("paged fixture");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .expect("sparse file");
    file.set_len(32 * 1024 * 1024 * 1024)
        .expect("sparse logical size");
    let error = salvage_embedded_store(&source, &destination).expect_err("unsupported format");
    assert!(error.to_string().contains("paged"), "{error}");
    assert!(!destination.exists());
}
