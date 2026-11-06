use super::*;

#[derive(Debug)]
pub struct PagedIndexSalvageFinding {
    pub collection: String,
    pub index: String,
    pub root_page: u32,
    pub reason: String,
}

#[derive(Debug)]
pub struct PagedIndexSalvageReport {
    pub indexes: Vec<PagedIndexSalvageFinding>,
}

impl PagedIndexSalvageReport {
    pub fn machine_json(&self) -> String {
        let indexes: Vec<_> = self
            .indexes
            .iter()
            .map(|finding| {
                crate::serde_json::json!({
                    "collection": finding.collection.clone(),
                    "index": finding.index.clone(),
                    "root_page": finding.root_page,
                    "reason": finding.reason.clone(),
                })
            })
            .collect();
        crate::serde_json::json!({
            "scope": "secondary_indexes_only",
            "action": "discarded_for_rebuild",
            "page_generation_freshness_verified": false,
            "indexes": indexes,
        })
        .to_string_compact()
    }
}

/// Recover a closed, checkpointed store when only derived secondary index
/// trees are damaged. Data and primary-key trees must verify. The output
/// retains index definitions but omits damaged physical roots so the runtime
/// can rebuild them from the surviving rows. Other corruption fails closed.
pub fn salvage_paged_indexes(source: &Path, destination: &Path) -> Result<PagedIndexSalvageReport> {
    let canonical_path = source.canonicalize()?;
    let source = canonical_path.as_path();
    // Hold the source lock across both verification and publication.
    let guard = Source::open(source)?;
    verify_supported_source(source, &guard)?;
    let scrub = scrub_paged_trees(source)?;
    let mut indexes = Vec::new();
    for finding in scrub.findings {
        let index = finding
            .tree
            .strip_prefix("secondary index ")
            .ok_or_else(|| {
                format!(
                    "cannot safely recover {} tree of {}: {}",
                    finding.tree, finding.collection, finding.reason
                )
            })?;
        indexes.push(PagedIndexSalvageFinding {
            collection: finding.collection,
            index: index.to_owned(),
            root_page: finding.root_page,
            reason: finding.reason,
        });
    }
    ensure(!indexes.is_empty(), "no damaged secondary index to recover")?;
    let report = PagedIndexSalvageReport { indexes };
    compact_paged_inner(source, destination, Some(&report), None)?;
    Ok(report)
}

#[derive(Debug)]
pub struct PagedCollectionLoss {
    pub collection: String,
    pub root_page: u32,
    pub skipped_pages: Vec<u32>,
}

#[derive(Debug)]
pub struct PagedCollectionSalvageReport {
    pub collections: Vec<PagedCollectionLoss>,
}

impl PagedCollectionSalvageReport {
    pub fn machine_json(&self) -> String {
        let collections: Vec<_> = self
            .collections
            .iter()
            .map(|loss| {
                crate::serde_json::json!({
                    "collection": loss.collection.clone(), "root_page": loss.root_page,
                    "action": if loss.skipped_pages == [loss.root_page] { "collection_emptied" } else { "damaged_leaves_skipped" },
                    "reason": "page_checksum_mismatch", "skipped_pages": loss.skipped_pages.clone(),
                    "skipped_entities": crate::serde_json::Value::Null,
                })
            })
            .collect();
        crate::serde_json::json!({"scope": "legacy_collection_trees", "page_generation_freshness_verified": false, "identity_high_water_verified": false, "safe_for_writes": false, "collections": collections}).to_string_compact()
    }
}

/// Recover intact collections and verified leaves in a reference-free legacy
/// store. Lost entity counts and identity high-water marks remain unknown.
pub fn salvage_paged_collections(
    source: &Path,
    destination: &Path,
) -> Result<PagedCollectionSalvageReport> {
    let canonical_path = source.canonicalize()?;
    let source = canonical_path.as_path();
    let guard = Arc::new(Source::open(source)?);
    verify_supported_source(source, &guard)?;
    ensure(
        guard.header.physical == f::PhysicalFileHeader::default(),
        "physical metadata requires separate data recovery verification",
    )?;
    ensure(
        !f::physical_metadata_binary_path(source).exists()
            && !f::physical_metadata_json_path(source).exists(),
        "metadata sidecar requires separate data recovery verification",
    )?;
    let catalog = Catalog::read(&guard)?;
    ensure(
        catalog.resume.is_none() && !catalog.has_cross_references(),
        "collection recovery requires a reference-free legacy catalog",
    )?;
    let scrub = scrub_paged_trees(source)?;
    let mut collections = Vec::new();
    for finding in scrub.findings {
        ensure(
            finding.tree == "collection",
            "only collection root damage is supported",
        )?;
        let canonical =
            reddb_engine::storage::system_collection_names::canonical_or_self(&finding.collection);
        ensure(
            !canonical.starts_with("red.") && !canonical.starts_with('_'),
            "system collections cannot be dropped",
        )?;
        let skipped_pages = match guard.raw_page(finding.root_page, true) {
            Ok(_) => {
                let tree = BTree::with_root(Arc::clone(&guard), finding.root_page);
                tree.visit_recoverable_leaves(
                    |id| {
                        guard.claim(id).map_err(|error| {
                            reddb_engine::storage::btree::BTreeError::Corrupted(error.to_string())
                        })
                    },
                    |_, _| Ok(()),
                )?
            }
            Err(error) => {
                ensure(
                    matches!(
                        error.downcast_ref::<reddb_engine::storage::page::PageError>(),
                        Some(reddb_engine::storage::page::PageError::ChecksumMismatch { .. })
                    ),
                    "collection root failure is not a checksum mismatch",
                )?;
                ensure(
                    guard.raw_page(finding.root_page, false)?.page_type()? == PageType::BTreeLeaf,
                    "only leaf-root collection damage is supported",
                )?;
                vec![finding.root_page]
            }
        };
        ensure(!skipped_pages.is_empty(), "no checksum-damaged leaf found")?;
        collections.push(PagedCollectionLoss {
            collection: finding.collection,
            root_page: finding.root_page,
            skipped_pages,
        });
    }
    ensure(
        !collections.is_empty(),
        "no damaged collection root to recover",
    )?;
    let report = PagedCollectionSalvageReport { collections };
    compact_paged_inner(source, destination, None, Some(&report))?;
    Ok(report)
}

pub(super) fn verify_supported_source(source: &Path, guard: &Source) -> Result<()> {
    guard.verify_superblocks()?;
    let operational = source.with_file_name(format!("{}.ops", f::data_file_name(source)));
    if operational.exists() {
        validate_operational(&operational)?;
        f::OperationalManifest::for_db_path(source).verify_marker_only_for_recovery()?;
    }
    for path in [
        f::logical_wal_path(source),
        f::legacy_logical_slots_path(source),
        f::legacy_audit_log_path(source),
    ] {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => ensure(
                metadata.is_file() && metadata.len() == 0,
                "logical WAL, slot or audit data requires separate recovery verification",
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let physical = guard.header.physical;
    for (page_id, expected) in [
        (physical.manifest_page, physical.manifest_checksum),
        (
            physical.collection_roots_page,
            physical.collection_roots_checksum,
        ),
        (physical.registry_page, physical.registry_checksum),
        (physical.recovery_page, physical.recovery_checksum),
        (physical.catalog_page, physical.catalog_checksum),
        (
            physical.metadata_state_page,
            physical.metadata_state_checksum,
        ),
        (
            physical.vector_artifact_page,
            physical.vector_artifact_checksum,
        ),
    ] {
        if page_id == 0 {
            continue;
        }
        let page = guard.raw_page(page_id, true)?;
        ensure(
            page.page_type()? == PageType::NativeMeta,
            "invalid physical metadata page",
        )?;
        // The published checksum covers the encoded payload, not the zero
        // padding to the page boundary. Its length is not stored separately.
        // Accept only a checksum matching a prefix with exclusively zero
        // bytes after it; the compactor also decodes each payload below.
        let content = page.content();
        let minimum = content
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(0, |position| position + 1);
        let mut checksum = crc32fast::Hasher::new();
        checksum.update(&content[..minimum]);
        let mut matches = u64::from(checksum.clone().finalize()) == expected;
        for byte in &content[minimum..] {
            checksum.update(std::slice::from_ref(byte));
            matches |= u64::from(checksum.clone().finalize()) == expected;
        }
        ensure(
            matches,
            "physical metadata differs from its published checksum",
        )?;
    }
    verify_vault(source, guard)?;
    Ok(())
}

pub(super) fn verify_vault(source: &Path, guard: &Source) -> Result<()> {
    let vault = guard.raw_page(2, false)?;
    if !vault.as_bytes().iter().all(|byte| *byte == 0) {
        ensure(vault.page_type()? == PageType::Vault, "invalid vault page")?;
        if vault.content().iter().all(|byte| *byte == 0) {
            vault.verify_checksum()?;
            return Ok(());
        }
        let content = vault.content();
        ensure(
            &content[..4] == b"RDVT" && matches!(content[4], 2 | 3),
            "unsupported vault header",
        )?;
        let payload = u32::from_le_bytes(content[21..25].try_into()?);
        ensure(
            payload <= 64 * 1024 * 1024,
            "vault exceeds verification budget",
        )?;
        let pager = Pager::open(
            source,
            PagerConfig {
                read_only: true,
                create: false,
                ..PagerConfig::default()
            },
        )?;
        let vault =
            crate::auth::vault::Vault::open(&pager).map_err(|_| "vault authentication failed")?;
        ensure(
            matches!(vault.load(&pager), Ok(Some(_))),
            "vault authentication failed",
        )?;
    }
    Ok(())
}
