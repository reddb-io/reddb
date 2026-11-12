//! Existing Rust header clients can update ordinary fields without replacing
//! independently published catalog or recovery authority.
use reddb_file::file_format::{
    database_header_dwb_publication, database_header_manifest_reference, decode_database_header,
    encode_database_header, set_database_header_dwb_publication,
    set_database_header_manifest_reference, DatabaseHeader, DatabaseHeaderError, DwbPhase,
    DwbPublication, HeaderPublicationError, PhysicalFileHeader, FILE_CAPABILITY_CATALOG_ROOT,
    FILE_CAPABILITY_DWB_PUBLICATION, PAGED_DWB_ENTRY_SIZE, PAGED_DWB_HEADER_SIZE, PAGED_PAGE_SIZE,
    PAGE_FILE_VERSION,
};
use reddb_file::page_proof::PageReference;

// An exhaustive match is part of the existing published Rust API contract.
fn classify_header_error(error: DatabaseHeaderError) -> &'static str {
    match error {
        DatabaseHeaderError::ShortPage { .. } => "short page",
        DatabaseHeaderError::InvalidMagic => "invalid magic",
        DatabaseHeaderError::UnsupportedPageSize(_) => "page size",
        DatabaseHeaderError::UnsupportedDatabaseVersion { .. } => "version",
        DatabaseHeaderError::UnsupportedRequiredCapabilities(_) => "capabilities",
    }
}

#[test]
fn existing_header_client_preserves_independently_published_authority() {
    // Keep the complete pre-extension field list: downstream struct literals
    // must continue compiling without adopting the new publication API.
    let mut header = DatabaseHeader {
        version: PAGE_FILE_VERSION,
        page_size: PAGED_PAGE_SIZE as u32,
        page_count: 100,
        freelist_head: 0,
        schema_version: 1,
        checkpoint_lsn: 7,
        checkpoint_in_progress: false,
        checkpoint_target_lsn: 0,
        required_capabilities: FILE_CAPABILITY_CATALOG_ROOT | FILE_CAPABILITY_DWB_PUBLICATION,
        physical: PhysicalFileHeader::default(),
    };
    let mut slot = vec![0; PAGED_PAGE_SIZE];
    encode_database_header(&mut slot, &header).expect("existing client creates header");
    let reference = PageReference {
        page_id: 70,
        digest: [3; 32],
    };
    let publication = DwbPublication {
        phase: DwbPhase::Ready,
        frame_bytes: (PAGED_DWB_HEADER_SIZE + PAGED_DWB_ENTRY_SIZE) as u32,
        digest: [4; 32],
    };
    set_database_header_manifest_reference(&mut slot, reference).expect("publish catalog");
    set_database_header_dwb_publication(&mut slot, Some(publication)).expect("publish recovery");
    header = decode_database_header(&slot)
        .map_err(classify_header_error)
        .expect("existing client reads ordinary header fields");
    header.checkpoint_lsn = 8;
    encode_database_header(&mut slot, &header).expect("existing client updates checkpoint");
    assert_eq!(
        decode_database_header(&slot).expect("updated header"),
        header
    );
    assert_eq!(
        database_header_manifest_reference(&slot).expect("catalog authority"),
        reference
    );
    assert_eq!(
        database_header_dwb_publication(&slot).expect("recovery authority"),
        Some(publication)
    );

    // A valid ordinary header cannot make malformed extension bytes valid.
    slot[256..292].fill(0);
    slot[256] = 70;
    assert!(matches!(
        database_header_manifest_reference(&slot),
        Err(HeaderPublicationError::InvalidManifestReference)
    ));
    slot[296..300].copy_from_slice(&99u32.to_le_bytes());
    assert!(matches!(
        database_header_dwb_publication(&slot),
        Err(HeaderPublicationError::InvalidDwbPublication)
    ));
}
