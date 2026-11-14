//! Publication-aware storage keeps the existing Rust header types and preserves
//! independent authority during ordinary field updates.
use reddb_file::file_format::{
    database_header_dwb_publication, database_header_manifest_reference, decode_database_header,
    decode_published_database_header, encode_database_header, set_database_header_dwb_publication,
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
fn existing_header_api_supports_publication_aware_updates() {
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
        required_capabilities: 0,
        physical: PhysicalFileHeader::default(),
    };
    let mut slot = vec![0; PAGED_PAGE_SIZE];
    encode_database_header(&mut slot, &header).expect("existing client creates header");
    header = decode_database_header(&slot)
        .map_err(classify_header_error)
        .expect("existing client reads its legacy file");
    header.checkpoint_lsn = 8;
    encode_database_header(&mut slot, &header).expect("existing client updates its legacy file");
    header.required_capabilities = FILE_CAPABILITY_CATALOG_ROOT | FILE_CAPABILITY_DWB_PUBLICATION;
    encode_database_header(&mut slot, &header).expect("new owner declares publication protocols");
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
    header = decode_published_database_header(&slot)
        .expect("participating owner validates publication before updating fields");
    header.checkpoint_lsn = 9;
    encode_database_header(&mut slot, &header).expect("participating owner updates checkpoint");
    assert_eq!(
        decode_published_database_header(&slot).expect("updated header"),
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

#[test]
fn legacy_decoder_rejects_publication_capabilities_it_cannot_expose() {
    // Old engines can depend on a newer compatible file crate while retaining
    // their fixed page-1 catalog and legacy DWB replay code. The unchanged API
    // must not silently authorize those readers to consume the new protocols.
    for capability in [
        FILE_CAPABILITY_CATALOG_ROOT,
        FILE_CAPABILITY_DWB_PUBLICATION,
        reddb_file::file_format::FILE_CAPABILITY_TREE_PROOFS,
        reddb_file::file_format::FILE_CAPABILITY_RECOVERY_READ_ONLY,
        reddb_file::file_format::FILE_CAPABILITY_BIOMETRIC_VALUES,
        reddb_file::file_format::FILE_CAPABILITY_FEATURE_VALUES,
        reddb_file::file_format::FILE_CAPABILITY_LANDMARK_VALUES,
    ] {
        let mut slot = vec![0; PAGED_PAGE_SIZE];
        encode_database_header(
            &mut slot,
            &DatabaseHeader {
                version: PAGE_FILE_VERSION,
                required_capabilities: capability,
                ..DatabaseHeader::default()
            },
        )
        .expect("new file protocol declaration");
        assert!(
            matches!(
                decode_database_header(&slot),
                Err(DatabaseHeaderError::UnsupportedRequiredCapabilities(bits)) if bits == capability
            ),
            "legacy decoder accepted unrepresented capability {capability:#x}"
        );
        assert_eq!(
            decode_published_database_header(&slot)
                .expect("participating reader admits a conservatively declared capability")
                .required_capabilities,
            capability
        );
    }
    let mut slot = vec![0; PAGED_PAGE_SIZE];
    encode_database_header(
        &mut slot,
        &DatabaseHeader {
            version: PAGE_FILE_VERSION,
            required_capabilities: 1 << 63,
            ..DatabaseHeader::default()
        },
    )
    .expect("future capability declaration");
    assert!(matches!(decode_published_database_header(&slot),
        Err(HeaderPublicationError::Header(DatabaseHeaderError::UnsupportedRequiredCapabilities(bits)))
        if bits == 1 << 63
    ));
}
