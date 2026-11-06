//! Offline native rewrite. Source is read-locked against writers and never
//! opened by a recovering writer. A new directory owns all output; the main
//! filename is published only after sync and a complete record-by-record
//! readback.
use crate::storage::engine::{Page, PageType, Pager, PagerConfig, HEADER_SIZE, PAGE_SIZE};
use reddb_engine::storage::{
    btree::BTree,
    pager_io::{DWB_ZONE_PAGES, DWB_ZONE_START_PAGE, FIRST_ALLOCATABLE_PAGE},
};
use reddb_file as f;
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::Arc,
};
mod assets;
mod catalog;
mod salvage;
mod scrub;
mod source;
use catalog::Catalog;
pub use salvage::{
    salvage_paged_collections, salvage_paged_indexes, PagedCollectionLoss,
    PagedCollectionSalvageReport, PagedIndexSalvageFinding, PagedIndexSalvageReport,
};
pub use scrub::{
    scrub_paged, scrub_paged_trees, PagedScrubReport, PagedTreeFinding, PagedTreeScrubReport,
};
use source::Source;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
pub struct CompactionReport {
    pub source_bytes: u64,
    pub destination_bytes: u64,
    pub source_allocated_bytes: Option<u64>,
    pub destination_allocated_bytes: Option<u64>,
    /// Includes exact persisted key and secondary-index records.
    pub records: u64,
    pub trees: usize,
    /// Historical stale backlinks reconstructed from validated forward/owner order.
    pub repaired_leaf_backlinks: u64,
}

impl CompactionReport {
    pub fn machine_json(&self) -> String {
        format!(
            r#"{{"source_bytes":{},"destination_bytes":{},"records":{},"trees":{},"repaired_leaf_backlinks":{},"source_allocated_bytes":{},"destination_allocated_bytes":{}}}"#,
            self.source_bytes,
            self.destination_bytes,
            self.records,
            self.trees,
            self.repaired_leaf_backlinks,
            self.source_allocated_bytes
                .map_or_else(|| "null".into(), |n| n.to_string()),
            self.destination_allocated_bytes
                .map_or_else(|| "null".into(), |n| n.to_string())
        )
    }
}

/// Rewrite a closed, checkpointed paged file into an exclusively-created output
/// directory. On failure the source is unchanged and the incomplete output is
/// retained for diagnosis. This does not switch a serving database or vacuum MVCC.
pub fn compact_paged(source: &Path, destination: &Path) -> Result<CompactionReport> {
    compact_paged_inner(source, destination, None, None, false)
}

/// Rewrite a closed store while explicitly retiring the obsolete physical
/// `red_config` identity. The canonical `red.config` tree is preserved as-is;
/// the source remains unchanged and ordinary compaction never enables this.
pub fn compact_paged_canonical_system_config(
    source: &Path,
    destination: &Path,
) -> Result<CompactionReport> {
    compact_paged_inner(source, destination, None, None, true)
}

/// Replay a supported pending unified WAL only against an isolated copy, then
/// publish a validated compacted database. The locked source is never opened
/// by a writer. Unsupported WAL families and ambiguous recovery fail closed.
pub fn recover_paged_wal(source: &Path, destination: &Path) -> Result<CompactionReport> {
    use crate::storage::wal::{WalReader, WalRecord};
    let source = source.canonicalize()?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    for owned in [
        f::support_dir_for(&source),
        source.with_file_name(format!("{}.ops", f::data_file_name(&source))),
    ] {
        if owned.exists() {
            ensure(
                !parent.starts_with(owned.canonicalize()?),
                "destination is inside source storage",
            )?;
        }
    }
    let guard = Source::open_for_wal_recovery(&source)?;
    guard.verify_superblocks()?;
    salvage::verify_standalone_sidecars(&source)?;
    let wal = f::unified_wal_path(&source);
    let engine_wal = f::engine_wal_path(&source);
    let pending_unified = wal_has_records(&wal)?;
    let pending_engine = wal_has_records(&engine_wal)?;
    ensure(
        pending_unified
            || pending_engine
            || guard.pending_double_write
            || guard.header.checkpoint_in_progress,
        "no pending recovery",
    )?;
    ensure(
        !guard.header.checkpoint_in_progress || pending_engine,
        "interrupted page checkpoint requires its engine WAL",
    )?;
    let mut pending_ownership = false;
    if pending_unified {
        for entry in WalReader::open(&wal)?.iter() {
            match entry?.1 {
                WalRecord::PageWrite { data, .. } => verify_wal_recovery_action(&data)?,
                WalRecord::TxCommitBatch { actions, .. } => {
                    for action in actions {
                        verify_wal_recovery_action(&action)?;
                    }
                }
                WalRecord::VectorInsert { .. } | WalRecord::ProbabilisticDelta { .. } => {
                    return Err(
                        "WAL contains runtime-owned data requiring separate recovery".into(),
                    );
                }
                WalRecord::PageOwnership { .. } | WalRecord::FullPageImage { .. } => {
                    pending_ownership = true
                }
                _ => {}
            }
        }
    }
    if !guard.pending_double_write
        && !guard.header.checkpoint_in_progress
        && !pending_ownership
        && !pending_engine
    {
        salvage::verify_vault(&source, &guard)?;
    }
    let mut recovered_engine_xid = 0;
    if pending_engine {
        for entry in WalReader::open(&engine_wal)?.iter() {
            match entry?.1 {
                WalRecord::Begin { .. }
                | WalRecord::Commit { .. }
                | WalRecord::Rollback { .. }
                | WalRecord::Checkpoint { .. } => {}
                WalRecord::PageWrite { page_id, data, .. } => {
                    ensure(
                        page_id > 0
                            && (page_id < DWB_ZONE_START_PAGE || page_id >= FIRST_ALLOCATABLE_PAGE)
                            && u64::from(page_id) < guard.bytes.div_ceil(PAGE_SIZE as u64)
                            && data.len() == PAGE_SIZE,
                        "invalid engine WAL page",
                    )?;
                    let page = Page::from_slice(&data)?;
                    ensure(
                        page.page_id() == page_id,
                        "engine WAL page identity mismatch",
                    )?;
                    page.page_type()?;
                    // Engine transactions log page bytes before Pager::write_page
                    // updates the page checksum. The WAL frame itself protects
                    // the image; redo will seal the page before writing it.
                }
                _ => return Err("unsupported engine WAL record".into()),
            }
        }
    }
    std::fs::create_dir(destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    }
    let stage = destination.join(".wal-replay");
    std::fs::create_dir(&stage)?;
    let name = source.file_name().ok_or("source filename is missing")?;
    let staged_source = stage.join(name);
    guard.copy_snapshot(&staged_source)?;
    copy_sidecars(&source, &staged_source)?;
    copy_physical_metadata_files(&source, &staged_source)?;
    let config = crate::storage::unified::UnifiedStoreConfig {
        resident_memory_bytes_max: (guard.header.version
            >= f::file_format::PAGE_DISK_RESIDENCY_FILE_VERSION)
            .then_some(64 * 1024 * 1024),
        ..Default::default()
    };
    if pending_engine {
        let pager = Pager::open(
            &staged_source,
            PagerConfig {
                disk_residency: config.resident_memory_bytes_max.is_some(),
                ..PagerConfig::default()
            },
        )?;
        let wal = f::engine_wal_path(&staged_source);
        recovered_engine_xid = replay_engine_wal_on_copy(&pager, &wal)?;
        let mut writer = crate::storage::wal::WalWriter::open(&wal)?;
        writer.truncate()?;
        writer.sync()?;
    }
    {
        let store =
            crate::storage::unified::UnifiedStore::open_with_config(&staged_source, config)?;
        if pending_engine || guard.pending_double_write {
            store.reconcile_physical_recovery()?;
            // The same root can now contain newer entity headers than the
            // old catalog summary. Preserve its previous floor, account for
            // the physical WAL, and visit the recovered base before publishing.
            let mut high = store
                .startup_xid_high_water_mark()
                .unwrap_or(0)
                .max(recovered_engine_xid);
            for name in store.list_collections() {
                let manager = store
                    .get_collection(&name)
                    .ok_or("recovered collection disappeared")?;
                let mut max_entity = 0u64;
                let mut max_row = 0u64;
                let mut max_sequence = 0u64;
                let mut has_entity = false;
                manager.try_for_each_entity(32 * 1024 * 1024, |entity| {
                    has_entity = true;
                    high = high.max(entity.xmin).max(entity.xmax);
                    max_entity = max_entity.max(entity.id.raw());
                    max_sequence = max_sequence.max(entity.sequence_id);
                    if let crate::storage::EntityKind::TableRow { row_id, .. } = &entity.kind {
                        max_row = max_row.max(*row_id);
                    }
                    true
                })?;
                if !has_entity {
                    continue;
                }
                let entity_next = max_entity
                    .checked_add(1)
                    .ok_or("recovered entity IDs are exhausted")?;
                let row_next = max_row
                    .checked_add(1)
                    .ok_or("recovered row IDs are exhausted")?;
                let sequence_next = max_sequence
                    .checked_add(1)
                    .ok_or("recovered sequence IDs are exhausted")?;
                manager.restore_identity_counters(entity_next, row_next);
                manager.restore_sequence_id_next(sequence_next);
                store.register_entity_id(crate::storage::EntityId::new(max_entity));
            }
            ensure(
                high < u64::MAX,
                "recovered transaction high-water mark is exhausted",
            )?;
            store.remember_startup_xid_high_water_mark(high);
        }
        store.persist()?;
    }
    let validated_dir = stage.join("validated");
    let report = compact_paged(&staged_source, &validated_dir)?;
    let verified = scrub_paged(&validated_dir.join(name))?;
    ensure(
        verified.assets_verified
            && verified.trees.findings.is_empty()
            && verified.unclassified_pages == 0,
        "replayed store failed scrub",
    )?;
    let final_path = destination.join(name);
    copy_sidecars(&validated_dir.join(name), &final_path)?;
    copy_physical_metadata_files(&validated_dir.join(name), &final_path)?;
    write_new(
        &destination.join("compaction.json"),
        report.machine_json().as_bytes(),
    )?;
    sync_directory(destination)?;
    std::fs::hard_link(validated_dir.join(name), &final_path)?;
    sync_directory(destination)?;
    // The published database must not retain a second pathname with a
    // different WAL family: future opens through that name would be unsafe.
    std::fs::remove_file(validated_dir.join(name))?;
    sync_directory(&validated_dir)?;
    Ok(report)
}

fn verify_wal_recovery_action(bytes: &[u8]) -> Result<()> {
    f::decode_store_wal_action_frame(bytes)?;
    Ok(())
}

fn wal_has_records(path: &Path) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len() > f::WAL_FILE_HEADER_BYTES as u64),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn replay_engine_wal_on_copy(pager: &Pager, path: &Path) -> Result<u64> {
    use crate::storage::wal::{WalReader, WalRecord};
    let mut transactions = std::collections::HashMap::new();
    let mut last_lsn = 0;
    let mut highest_tx = 0u64;
    let header = pager.header()?;
    let mut target_seen = !header.checkpoint_in_progress;
    for item in WalReader::open(path)?.iter() {
        let (lsn, record) = item?;
        last_lsn = lsn;
        target_seen |= lsn == header.checkpoint_target_lsn;
        match record {
            WalRecord::Begin { tx_id } => {
                highest_tx = highest_tx.max(tx_id);
                transactions.insert(tx_id, false);
            }
            WalRecord::Commit { tx_id } => {
                highest_tx = highest_tx.max(tx_id);
                transactions.insert(tx_id, true);
            }
            WalRecord::Rollback { tx_id } => {
                highest_tx = highest_tx.max(tx_id);
                transactions.remove(&tx_id);
            }
            WalRecord::PageWrite { tx_id, .. } => highest_tx = highest_tx.max(tx_id),
            _ => {}
        }
    }
    ensure(
        target_seen,
        "engine WAL does not contain the interrupted checkpoint target",
    )?;
    // Store only the LSN of each latest committed page, not every page image.
    let mut latest = std::collections::HashMap::new();
    for item in WalReader::open(path)?.iter() {
        let (lsn, record) = item?;
        if let WalRecord::PageWrite { tx_id, page_id, .. } = record {
            if transactions.get(&tx_id).copied().unwrap_or(false) {
                latest.insert(page_id, lsn);
            }
        }
    }
    ensure(
        !header.checkpoint_in_progress || !latest.is_empty(),
        "interrupted checkpoint has no committed page images",
    )?;
    if !latest.is_empty() {
        pager.set_checkpoint_in_progress(true, last_lsn)?;
        for item in WalReader::open(path)?.iter() {
            let (lsn, record) = item?;
            if let WalRecord::PageWrite { page_id, data, .. } = record {
                if latest.get(&page_id) == Some(&lsn) {
                    pager.write_page(page_id, Page::from_slice(&data)?)?;
                }
            }
        }
        pager.sync()?;
        pager.complete_checkpoint(last_lsn)?;
    }
    Ok(highest_tx)
}

fn copy_physical_metadata_files(source: &Path, destination: &Path) -> Result<()> {
    for (old, new) in [
        (
            f::physical_metadata_binary_path(source),
            f::physical_metadata_binary_path(destination),
        ),
        (
            f::physical_metadata_json_path(source),
            f::physical_metadata_json_path(destination),
        ),
    ] {
        match std::fs::symlink_metadata(&old) {
            Ok(stat) => {
                ensure(
                    stat.is_file() && stat.len() <= 64 * 1024 * 1024,
                    "invalid physical metadata sidecar",
                )?;
                let mut input = File::open(&old)?;
                let mut output = create_new(&new)?;
                std::io::copy(&mut input, &mut output)?;
                output.sync_all()?;
                drop(output);
                verify_file_copy(&old, &new)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn compact_paged_inner(
    source: &Path,
    destination: &Path,
    salvage: Option<&PagedIndexSalvageReport>,
    lost_collections: Option<&PagedCollectionSalvageReport>,
    retain_canonical_system_config: bool,
) -> Result<CompactionReport> {
    let source_path = source.canonicalize()?;
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    for source_directory in [
        f::support_dir_for(&source_path),
        source_path.with_file_name(format!("{}.ops", f::data_file_name(&source_path))),
    ] {
        if source_directory.exists() {
            ensure(
                !parent.starts_with(source_directory.canonicalize()?),
                "destination is inside source storage",
            )?;
        }
    }
    let source = Arc::new(Source::open(&source_path)?);
    let mut catalog = Catalog::read(&source)?;
    if retain_canonical_system_config {
        catalog.retain_canonical_system_config()?;
    }
    ensure(
        source.header.version < f::file_format::PAGE_DISK_RESIDENCY_FILE_VERSION
            || catalog.resume.is_some(),
        "prepared disk-resident file has no resume catalog",
    )?;
    if let Some(salvage) = salvage {
        let resume = catalog
            .resume
            .as_mut()
            .ok_or("index recovery requires a resume catalog")?;
        for finding in &salvage.indexes {
            let state = resume
                .collections
                .get_mut(&finding.collection)
                .ok_or("index collection missing from resume")?;
            let index = state
                .secondary_indices
                .remove(&finding.index)
                .ok_or("index missing from resume")?;
            ensure(
                index.root_page == finding.root_page,
                "index root changed during salvage",
            )?;
        }
    }
    if let Some(losses) = lost_collections {
        ensure(
            catalog.resume.is_none() && !catalog.has_cross_references(),
            "collection recovery requires a reference-free legacy catalog",
        )?;
        for loss in &losses.collections {
            let root = catalog
                .roots
                .iter_mut()
                .find(|root| root.collection == loss.collection)
                .ok_or("collection missing from catalog")?;
            ensure(
                root.root_page == loss.root_page,
                "collection root changed during salvage",
            )?;
            if loss.skipped_pages == [loss.root_page] {
                root.root_page = 0;
            }
        }
    }
    let name = source_path
        .file_name()
        .ok_or("source filename is missing")?;
    // Exclusive creation prevents clobbering files, symlinks and other runs.
    std::fs::create_dir(destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    }
    let output = destination.join(name);
    let temporary = destination.join(".compacting.rdb");
    ensure(output != temporary, "reserved compaction source filename")?;
    let target = Arc::new(Pager::open(
        &temporary,
        PagerConfig {
            cache_size: 1024,
            disk_residency: source.header.version
                >= f::file_format::PAGE_DISK_RESIDENCY_FILE_VERSION,
            ..PagerConfig::default()
        },
    )?);
    let mut roots = BTreeMap::new();
    let mut recovered_trees = BTreeMap::new();
    let mut records = 0;
    let mut repaired_leaf_backlinks = 0;
    for collection in &catalog.roots {
        let loss = lost_collections.and_then(|report| {
            report
                .collections
                .iter()
                .find(|loss| loss.collection == collection.collection && collection.root_page != 0)
        });
        let count = if let Some(loss) = loss {
            let (count, hash) = rewrite_recoverable_tree(&source, &target, loss, &mut roots)?;
            recovered_trees.insert(loss.root_page, (count, hash));
            count
        } else {
            rewrite_tree(
                &source,
                &target,
                collection.root_page,
                &mut roots,
                &mut repaired_leaf_backlinks,
            )?
        };
        if let Some(resume) = &catalog.resume {
            ensure(
                resume.collections[&collection.collection].entities == count,
                "collection count disagrees with resume",
            )?;
        }
        records += count;
    }
    if let Some(resume) = &catalog.resume {
        for state in resume.collections.values() {
            if let Some(root) = state.key_index_root {
                records += rewrite_tree(
                    &source,
                    &target,
                    root,
                    &mut roots,
                    &mut repaired_leaf_backlinks,
                )?;
            }
            for index in state.secondary_indices.values() {
                records += rewrite_tree(
                    &source,
                    &target,
                    index.root_page,
                    &mut roots,
                    &mut repaired_leaf_backlinks,
                )?;
            }
        }
    }
    let mut assets = assets::Assets::new(&source, &target);
    assets.vault()?;
    let physical = assets.physical()?;
    assets.metadata(&source_path, Some(&output))?;
    if salvage.is_some() || lost_collections.is_some() {
        if let Some(salvage) = salvage {
            for index in &salvage.indexes {
                if index.root_page != 0 {
                    match source.raw_page(index.root_page, true) {
                        Ok(_) => {
                            let tree = BTree::with_root(Arc::clone(&source), index.root_page);
                            let skipped = tree.visit_recoverable_leaves(
                                |id| {
                                    source.claim(id).map_err(|error| {
                                        reddb_engine::storage::btree::BTreeError::Corrupted(
                                            error.to_string(),
                                        )
                                    })
                                },
                                |_, _| Ok(()),
                            )?;
                            ensure(
                                !skipped.is_empty(),
                                "index damage is not a verified leaf checksum failure",
                            )?;
                        }
                        Err(error) => {
                            ensure(
                                matches!(
                                    error.downcast_ref::<reddb_engine::storage::page::PageError>(),
                                    Some(
                                        reddb_engine::storage::page::PageError::ChecksumMismatch { .. }
                                    )
                                ),
                                "index root failure is not a checksum mismatch",
                            )?;
                            ensure(
                                source.raw_page(index.root_page, false)?.page_type()?
                                    == PageType::BTreeLeaf,
                                "damaged interior index root cannot be recovered",
                            )?;
                            source.claim(index.root_page)?;
                        }
                    }
                }
            }
        }
        if let Some(losses) = lost_collections {
            for collection in &losses.collections {
                if collection.skipped_pages == [collection.root_page] {
                    source.claim(collection.root_page)?;
                }
            }
        }
        ensure(
            source.unclaimed_pages() == 0,
            "unclassified source pages require separate recovery verification",
        )?;
    }
    catalog::write_catalog(&target, &catalog.encode(&roots)?)?;
    target.update_physical_header(physical)?;
    target.fence_file_format_version(source.header.version)?;
    target.sync()?;
    // Preserve source schema/LSN, but retain the newly allocated page boundary.
    for _ in 0..2 {
        target.commit_superblock(|slot| {
            let mut header = f::decode_database_header(slot).map_err(|error| {
                reddb_engine::storage::pager::PagerError::InvalidDatabase(error.to_string())
            })?;
            header.schema_version = source.header.schema_version;
            header.checkpoint_lsn = source.header.checkpoint_lsn;
            f::encode_database_header(slot, &header).map_err(|error| {
                reddb_engine::storage::pager::PagerError::InvalidDatabase(error.to_string())
            })
        })?;
    }
    if lost_collections.is_some() {
        // Both copies must be fenced before the recovered filename is published.
        // Old engines reject this newer version; this engine admits it read-only.
        for _ in 0..2 {
            target.commit_superblock(|slot| {
                f::set_database_header_version(
                    slot,
                    f::file_format::PAGE_RECOVERY_READ_ONLY_FILE_VERSION,
                )
                .map_err(|error| {
                    reddb_engine::storage::pager::PagerError::InvalidDatabase(error.to_string())
                })
            })?;
        }
    }
    let assets_verification = assets.into_verification();
    drop(target);
    let reopened = Arc::new(Source::open(&temporary)?);
    if salvage.is_some() || lost_collections.is_some() {
        salvage::verify_vault(&temporary, &reopened)?;
    }
    ensure(
        reopened.header.physical == physical,
        "rewritten physical header failed parity",
    )?;
    let rewritten_catalog = Catalog::read(&reopened)?;
    ensure(
        rewritten_catalog.encode(&identity_roots(&roots))? == catalog.encode(&roots)?,
        "rewritten catalog differs from source",
    )?;
    for (&old, &new) in &roots {
        if let Some((count, hash)) = recovered_trees.get(&old) {
            verify_recovered_tree(&reopened, new, *count, *hash)?;
        } else {
            verify_tree(&source, &reopened, old, new)?;
        }
    }
    for (id, expected, checksum) in assets_verification {
        let page = reopened.raw_page(id, checksum)?;
        if id != 2 {
            reopened.claim(id)?;
        }
        ensure(
            blake3::hash(page.content()) == expected,
            "native asset failed readback parity",
        )?;
    }
    let report = CompactionReport {
        source_bytes: source.bytes,
        destination_bytes: reopened.bytes,
        source_allocated_bytes: allocated_bytes(&source_path)?,
        destination_allocated_bytes: allocated_bytes(&temporary)?,
        records,
        trees: roots.len(),
        repaired_leaf_backlinks,
    };
    drop(reopened);
    copy_sidecars(&source_path, &output)?;
    if retain_canonical_system_config {
        let operational_path = output.with_file_name(format!("{}.ops", f::data_file_name(&output)));
        if operational_path.exists() {
            crate::storage::operational_manifest::OperationalManifest::for_db_path(&output)
                .reconcile_collection_collisions(&[("red.config", "red_config", "red.config")])?;
        }
    }
    write_new(
        &destination.join("compaction.json"),
        report.machine_json().as_bytes(),
    )?;
    if let Some(salvage) = salvage {
        write_new(
            &destination.join("salvage.json"),
            salvage.machine_json().as_bytes(),
        )?;
    }
    if let Some(losses) = lost_collections {
        write_new(
            &destination.join("salvage.json"),
            losses.machine_json().as_bytes(),
        )?;
    }
    sync_directory(destination)?;
    // hard_link is create-new even if an unexpected file appears during work.
    std::fs::hard_link(&temporary, &output)?;
    sync_directory(destination)?;
    std::fs::remove_file(&temporary)?;
    sync_directory(destination)?;
    if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
        sync_directory(parent)?;
    }
    Ok(report)
}
fn identity_roots(roots: &BTreeMap<u32, u32>) -> BTreeMap<u32, u32> {
    roots.values().map(|root| (*root, *root)).collect()
}

fn hash_record(hasher: &mut blake3::Hasher, key: &[u8], value: &[u8]) {
    hasher.update(&(key.len() as u64).to_le_bytes());
    hasher.update(key);
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn rewrite_recoverable_tree(
    source: &Arc<Source>,
    target: &Arc<Pager>,
    loss: &PagedCollectionLoss,
    roots: &mut BTreeMap<u32, u32>,
) -> Result<(u64, blake3::Hash)> {
    ensure(
        !roots.contains_key(&loss.root_page),
        "duplicate live tree owner",
    )?;
    let original = BTree::with_root(Arc::clone(source), loss.root_page);
    let destination = BTree::new(Arc::clone(target));
    let mut hash = blake3::Hasher::new();
    let mut count = 0u64;
    let mut batch = Vec::new();
    let mut size = 0usize;
    let skipped = original.visit_recoverable_leaves(
        |id| {
            source.claim(id).map_err(|error| {
                reddb_engine::storage::btree::BTreeError::Corrupted(error.to_string())
            })
        },
        |key, value| {
            hash_record(&mut hash, &key, &value);
            size += key.len() + value.len();
            batch.push((key, value));
            count += 1;
            if size >= 1024 * 1024 || batch.len() >= 512 {
                destination.bulk_insert_sorted(&batch)?;
                batch.clear();
                size = 0;
            }
            Ok(())
        },
    )?;
    ensure(
        skipped == loss.skipped_pages,
        "damaged pages changed during recovery",
    )?;
    destination.bulk_insert_sorted(&batch)?;
    roots.insert(loss.root_page, destination.root_page_id());
    Ok((count, hash.finalize()))
}

fn verify_recovered_tree(
    source: &Arc<Source>,
    root: u32,
    count: u64,
    expected: blake3::Hash,
) -> Result<()> {
    let tree = BTree::with_root(Arc::clone(source), root);
    let (owned, _) = inventory_tree(source, &tree, false)?;
    ensure(owned == count, "recovered tree record count differs")?;
    let mut cursor = tree.cursor_first()?;
    let mut hash = blake3::Hasher::new();
    let mut actual = 0u64;
    while let Some((key, value)) = cursor.next_with_budget(32 * 1024 * 1024)? {
        hash_record(&mut hash, &key, &value);
        actual += 1;
        ensure(actual <= count, "recovered tree has extra records")?;
    }
    ensure(
        actual == count && hash.finalize() == expected,
        "recovered tree failed record parity",
    )
}

fn rewrite_tree(
    source: &Arc<Source>,
    target: &Arc<Pager>,
    root: u32,
    roots: &mut BTreeMap<u32, u32>,
    repaired_leaf_backlinks: &mut u64,
) -> Result<u64> {
    if root == 0 {
        return Ok(0);
    }
    ensure(!roots.contains_key(&root), "duplicate live tree owner")?;
    let tree = BTree::with_root(Arc::clone(source), root);
    let (expected, repairs) = inventory_tree(source, &tree, true)?;
    *repaired_leaf_backlinks += repairs;
    let destination = BTree::new(Arc::clone(target));
    let mut cursor = tree.cursor_first()?;
    let mut count = 0;
    let mut previous: Option<Vec<u8>> = None;
    let mut batch = Vec::new();
    let mut bytes = 0;
    while let Some((key, value)) = cursor.next_with_budget(32 * 1024 * 1024)? {
        ensure(
            previous.as_ref().is_none_or(|old| old < &key),
            "source tree keys are not strictly ordered",
        )?;
        previous = Some(key.clone());
        bytes += key.len() + value.len();
        batch.push((key, value));
        count += 1;
        ensure(
            count <= expected,
            "tree cursor exceeds structural inventory",
        )?;
        if bytes >= 1024 * 1024 || batch.len() >= 512 {
            destination.bulk_insert_sorted(&batch)?;
            batch.clear();
            bytes = 0;
        }
    }
    destination.bulk_insert_sorted(&batch)?;
    ensure(count == expected, "tree cursor omits owned records")?;
    roots.insert(root, destination.root_page_id());
    Ok(count)
}
// Interior ownership and forward traversal must agree exactly. Old split code
// omitted the successor backlink update; that redundant edge can be rebuilt
// without guessing any ownership or key/value. Never repair forward omissions.
fn inventory_tree(
    source: &Source,
    tree: &BTree<Source>,
    allow_backlinks: bool,
) -> Result<(u64, u64)> {
    let pages = tree.retired_pages()?;
    let mut leaves = Vec::new();
    let mut expected = 0u64;
    for id in pages {
        source.claim(id)?;
        let page = source.raw_page(id, true)?;
        if page.page_type()? == PageType::BTreeLeaf {
            leaves.push((id, number(page.content(), 0)?, number(page.content(), 4)?));
            expected += u64::from(page.cell_count());
        }
    }
    // Ownership traversal is right-to-left. A cursor follows the independent
    // leaf chain; prove it visits exactly the structurally owned leaves.
    leaves.reverse();
    let mut repaired = 0;
    for (index, &(id, previous, next)) in leaves.iter().enumerate() {
        ensure(
            next == leaves.get(index + 1).map_or(0, |leaf| leaf.0),
            &format!(
                "broken forward leaf chain at page {id}, root {}",
                tree.root_page_id()
            ),
        )?;
        if previous != index.checked_sub(1).map_or(0, |i| leaves[i].0) {
            ensure(
                allow_backlinks,
                &format!("broken backward leaf chain at page {id}"),
            )?;
            repaired += 1;
        }
    }
    Ok((expected, repaired))
}
fn verify_tree(source: &Arc<Source>, destination: &Arc<Source>, old: u32, new: u32) -> Result<()> {
    let old = BTree::with_root(Arc::clone(source), old);
    let new = BTree::with_root(Arc::clone(destination), new);
    inventory_tree(destination, &new, false)?;
    let mut left = old.cursor_first()?;
    let mut right = new.cursor_first()?;
    loop {
        let expected = left.next_with_budget(32 * 1024 * 1024)?;
        let actual = right.next_with_budget(32 * 1024 * 1024)?;
        ensure(
            expected == actual,
            "compacted tree failed complete key/value parity",
        )?;
        if expected.is_none() {
            return Ok(());
        }
    }
}
fn copy_sidecars(source: &Path, destination: &Path) -> Result<()> {
    // These files contain logical records, not physical page addresses. Empty
    // checkpointed WALs also preserve their version exactly.
    for (old, new) in [
        (
            f::unified_wal_path(source),
            f::unified_wal_path(destination),
        ),
        (f::engine_wal_path(source), f::engine_wal_path(destination)),
        (
            f::logical_wal_path(source),
            f::logical_wal_path(destination),
        ),
        (
            f::legacy_logical_slots_path(source),
            f::legacy_logical_slots_path(destination),
        ),
        (
            f::legacy_audit_log_path(source),
            f::legacy_audit_log_path(destination),
        ),
    ] {
        match File::open(&old) {
            Ok(mut input) => {
                let mut output = create_new(&new)?;
                std::io::copy(&mut input, &mut output)?;
                output.sync_all()?;
                verify_file_copy(&old, &new)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let support = f::support_dir_for(source);
    if support.exists() {
        validate_empty_directory(&support, 0)?;
        copy_directory(&support, &f::support_dir_for(destination), 0)?;
    }
    let old = source.with_file_name(format!("{}.ops", f::data_file_name(source)));
    let new = destination.with_file_name(format!("{}.ops", f::data_file_name(destination)));
    if old.exists() {
        validate_operational(&old)?;
        copy_directory(&old, &new, 0)?;
    }
    Ok(())
}
fn copy_directory(source: &Path, destination: &Path, depth: usize) -> Result<()> {
    ensure(depth < 32, "operational directory nesting exceeds limit")?;
    ensure(
        std::fs::symlink_metadata(source)?.is_dir(),
        "operational directory must not be a symlink",
    )?;
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let output = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_directory(&entry.path(), &output, depth + 1)?;
        } else {
            ensure(
                kind.is_file(),
                "operational directory contains a symlink or special file",
            )?;
            let mut input = File::open(entry.path())?;
            let mut target = create_new(&output)?;
            std::io::copy(&mut input, &mut target)?;
            target.sync_all()?;
            verify_file_copy(&entry.path(), &output)?;
        }
    }
    sync_directory(destination)
}
fn validate_empty_directory(path: &Path, depth: usize) -> Result<()> {
    ensure(depth < 32, "support directory nesting exceeds limit")?;
    ensure(
        std::fs::symlink_metadata(path)?.is_dir(),
        "support directory must not be a symlink",
    )?;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        ensure(
            entry.file_type()?.is_dir(),
            "non-empty support layout requires separate compaction qualification",
        )?;
        validate_empty_directory(&entry.path(), depth + 1)?;
    }
    Ok(())
}
fn validate_operational(root: &Path) -> Result<()> {
    ensure(
        !root.join("forks").exists(),
        "store forks require separate compaction qualification",
    )?;
    let manifest = root.join("manifest.json");
    ensure(
        std::fs::metadata(&manifest)?.len() <= 64 * 1024 * 1024,
        "operational manifest exceeds budget",
    )?;
    let document: crate::serde_json::Value =
        crate::serde_json::from_str(&std::fs::read_to_string(manifest)?)?;
    fn check(value: &crate::serde_json::Value, depth: usize) -> Result<()> {
        use crate::serde_json::Value;
        ensure(depth < 32, "operational manifest exceeds nesting limit")?;
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "source" || key == "fork_origin" {
                        ensure(
                            matches!(value, Value::Null),
                            "external operational owners require separate qualification",
                        )?;
                    }
                    if key == "path" {
                        let path = Path::new(value.as_str().ok_or("non-string operational path")?);
                        ensure(
                            !path.is_absolute()
                                && path
                                    .components()
                                    .all(|part| matches!(part, std::path::Component::Normal(_))),
                            "operational path escapes its directory",
                        )?;
                    }
                    check(value, depth + 1)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    check(value, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    check(&document, 0)
}
fn verify_file_copy(source: &Path, destination: &Path) -> Result<()> {
    ensure(
        hash_reader(&mut File::open(source)?)? == hash_reader(&mut File::open(destination)?)?,
        "sidecar failed complete readback parity",
    )
}
fn hash_reader(input: &mut impl Read) -> Result<blake3::Hash> {
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize())
}
fn allocated_bytes(path: &Path) -> Result<Option<u64>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Some(
            std::fs::metadata(path)?
                .blocks()
                .checked_mul(512)
                .ok_or("allocated byte count overflow")?,
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(None)
    }
}
fn create_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = create_new(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn sync_directory(_path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(_path)?.sync_all()?;
    Ok(())
}
fn ensure(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, message).into())
    }
}
fn number(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or("truncated native integer")?
            .try_into()?,
    ))
}
