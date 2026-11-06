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
    compact_paged_inner(source, destination, Some(&report))?;
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
    let vault = guard.raw_page(2, false)?;
    if !vault.as_bytes().iter().all(|byte| *byte == 0) {
        ensure(
            vault.page_type()? == PageType::Vault && vault.content().iter().all(|byte| *byte == 0),
            "authenticated vault recovery requires vault verification",
        )?;
        vault.verify_checksum()?;
    }
    Ok(())
}
