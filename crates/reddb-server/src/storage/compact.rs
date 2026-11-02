//! Offline native rewrite. Source is exclusively locked and never opened by a
//! recovering writer. A new directory owns all output; the main filename is
//! published only after sync and a complete record-by-record readback.
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
mod source;
use catalog::Catalog;
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
}

impl CompactionReport {
    pub fn machine_json(&self) -> String {
        format!(
            r#"{{"source_bytes":{},"destination_bytes":{},"records":{},"trees":{},"source_allocated_bytes":{},"destination_allocated_bytes":{}}}"#,
            self.source_bytes,
            self.destination_bytes,
            self.records,
            self.trees,
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
    let source_path = source.canonicalize()?;
    let source = Arc::new(Source::open(&source_path)?);
    let catalog = Catalog::read(&source)?;
    ensure(
        source.header.version < f::file_format::PAGE_DISK_RESIDENCY_FILE_VERSION
            || catalog.resume.is_some(),
        "prepared disk-resident file has no resume catalog",
    )?;
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
    let mut records = 0;
    for collection in &catalog.roots {
        let count = rewrite_tree(&source, &target, collection.root_page, &mut roots)?;
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
                records += rewrite_tree(&source, &target, root, &mut roots)?;
            }
            for index in state.secondary_indices.values() {
                records += rewrite_tree(&source, &target, index.root_page, &mut roots)?;
            }
        }
    }
    let mut assets = assets::Assets::new(&source, &target);
    assets.vault()?;
    let physical = assets.physical()?;
    assets.metadata(&source_path, &output)?;
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
    let assets_verification = assets.into_verification();
    drop(target);
    let reopened = Arc::new(Source::open(&temporary)?);
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
        verify_tree(&source, &reopened, old, new)?;
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
    };
    drop(reopened);
    copy_sidecars(&source_path, &output)?;
    write_new(
        &destination.join("compaction.json"),
        report.machine_json().as_bytes(),
    )?;
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

fn rewrite_tree(
    source: &Arc<Source>,
    target: &Arc<Pager>,
    root: u32,
    roots: &mut BTreeMap<u32, u32>,
) -> Result<u64> {
    if root == 0 {
        return Ok(0);
    }
    ensure(!roots.contains_key(&root), "duplicate live tree owner")?;
    let tree = BTree::with_root(Arc::clone(source), root);
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
    for (index, &(id, previous, next)) in leaves.iter().enumerate() {
        ensure(
            previous == index.checked_sub(1).map_or(0, |i| leaves[i].0)
                && next == leaves.get(index + 1).map_or(0, |leaf| leaf.0),
            &format!("broken leaf chain at page {id}"),
        )?;
    }
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
fn verify_tree(source: &Arc<Source>, destination: &Arc<Source>, old: u32, new: u32) -> Result<()> {
    let old = BTree::with_root(Arc::clone(source), old);
    let new = BTree::with_root(Arc::clone(destination), new);
    for page in new.retired_pages()? {
        destination.claim(page)?;
    }
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
    fn hash(path: &Path) -> Result<blake3::Hash> {
        let mut file = File::open(path)?;
        let mut hash = blake3::Hasher::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        Ok(hash.finalize())
    }
    ensure(
        hash(source)? == hash(destination)?,
        "sidecar failed complete readback parity",
    )
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
fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
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
