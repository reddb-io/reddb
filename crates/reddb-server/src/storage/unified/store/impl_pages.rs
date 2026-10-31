use super::*;
use parking_lot::RwLock;

const CHECKPOINT_BATCH_BYTES: usize = 1024 * 1024;
const CHECKPOINT_BATCH_ROWS: usize = 512;

/// Keep encoded payload memory proportional to a small batch, not a collection.
/// A single large value retains the existing B-tree value-size contract.
struct CheckpointTreeBuilder {
    tree: Arc<BTree>,
    records: Vec<(Vec<u8>, Vec<u8>)>,
    bytes: usize,
}

impl CheckpointTreeBuilder {
    fn new(pager: &Arc<Pager>) -> Self {
        Self {
            tree: Arc::new(BTree::new(Arc::clone(pager))),
            records: Vec::new(),
            bytes: 0,
        }
    }

    fn push(&mut self, record: (Vec<u8>, Vec<u8>)) -> Result<(), StoreError> {
        let bytes = record.0.len() + record.1.len();
        if !self.records.is_empty()
            && (self.bytes.saturating_add(bytes) > CHECKPOINT_BATCH_BYTES
                || self.records.len() >= CHECKPOINT_BATCH_ROWS)
        {
            self.flush()?;
        }
        self.bytes += bytes;
        self.records.push(record);
        #[cfg(test)]
        CHECKPOINT_BUFFERED_BYTES.with(|peak| peak.set(peak.get().max(self.bytes)));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StoreError> {
        self.tree
            .bulk_insert_sorted(&self.records)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        self.records.clear();
        self.bytes = 0;
        Ok(())
    }

    fn copy_prefix(&mut self, previous: &BTree, count: usize) -> Result<(), StoreError> {
        let mut cursor = previous
            .cursor_first()
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        for _ in 0..count {
            let record = cursor
                .next()
                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?
                .ok_or_else(|| StoreError::Serialization("checkpoint prefix disappeared".into()))?;
            self.push(record)?;
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static CHECKPOINT_SERIALIZED_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static REPLAY_SERIALIZED_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CHECKPOINT_BUFFERED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn checkpoint_visibility_matches(
    previous: &CheckpointVersion,
    current: &CheckpointVersion,
) -> bool {
    (previous.visibility_revision == current.visibility_revision
        || previous.excluded_xids == current.excluded_xids)
        && previous.pending_vault == current.pending_vault
}

// ── Pager-meta overflow chain (gh-477) ──────────────────────────────────────
// When the serialized collection registry + cross-refs exceed a single page,
// page 1 carries a native metadata-overflow header pointing at an overflow chain of
// `PageType::Overflow` pages. Single-page metadata keeps the historical
// bit-identical layout (`METADATA_MAGIC = "RDM2"` written directly at the
// content offset).
//
// Page 1 (overflow form), starting at `HEADER_SIZE`:
//   [0..4]   native metadata-overflow magic
//   [4..8]   format_version (u32, mirrors inner payload version for debug)
//   [8..12]  total_payload_bytes (u32)
//   [12..16] next_overflow_page_id (u32, > 0)
//   [16..]   first payload chunk (up to META_V3_FIRST_PAYLOAD_CAP bytes)
//
// Overflow continuation page, starting at `HEADER_SIZE`:
//   [0..4]   next_overflow_page_id (u32, 0 if last)
//   [4..8]   chunk_bytes (u32)
//   [8..]    chunk payload (up to META_V3_OVERFLOW_PAYLOAD_CAP bytes)
const META_PAGE_CONTENT_CAP: usize =
    crate::storage::engine::PAGE_SIZE - crate::storage::engine::HEADER_SIZE;
const META_V3_PAGE1_HEADER: usize = reddb_file::METADATA_OVERFLOW_HEADER_BYTES;
const META_V3_OVERFLOW_HEADER: usize = reddb_file::METADATA_OVERFLOW_CONTINUATION_HEADER_BYTES;
const META_V3_FIRST_PAYLOAD_CAP: usize = META_PAGE_CONTENT_CAP - META_V3_PAGE1_HEADER;
const META_V3_OVERFLOW_PAYLOAD_CAP: usize = META_PAGE_CONTENT_CAP - META_V3_OVERFLOW_HEADER;

fn existing_overflow_chain(pager: &Pager) -> Result<Vec<u32>, PagerError> {
    let cs = crate::storage::engine::HEADER_SIZE;
    let page = pager.read_page(1)?;
    let bytes = page.as_bytes();
    if bytes.len() < cs + META_V3_PAGE1_HEADER {
        return Ok(Vec::new());
    }
    let Some(header) =
        reddb_file::decode_native_metadata_overflow_header(&bytes[cs..]).map_err(|err| {
            PagerError::InvalidDatabase(format!("invalid metadata overflow header: {err}"))
        })?
    else {
        return Ok(Vec::new());
    };
    let mut next = header.next_overflow_page_id;
    let mut pages = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while next != 0 {
        if !seen.insert(next) || pages.len() >= pager.page_count()? as usize {
            return Err(PagerError::InvalidDatabase(
                "cyclic metadata overflow chain".into(),
            ));
        }
        let ov = pager.read_page(next)?;
        if ov.page_type()? != crate::storage::engine::PageType::Overflow {
            return Err(PagerError::InvalidDatabase(
                "invalid metadata overflow page type".into(),
            ));
        }
        let ob = ov.as_bytes();
        let nn = reddb_file::decode_native_metadata_overflow_continuation_header(&ob[cs..])
            .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?
            .next_overflow_page_id;
        pages.push(next);
        next = nn;
    }
    Ok(pages)
}

fn publish_paged_metadata(pager: &Pager, meta_data: &[u8]) -> Result<(), PagerError> {
    let retired = existing_overflow_chain(pager)?;
    if !retired.is_empty() {
        pager.record_retirement_intent(&retired)?;
    }
    let page = build_meta_page1_with_overflow(pager, meta_data)?;
    // Cache eviction order must not publish a root before its children.
    pager.sync()?;
    pager.write_page(1, page)?;
    pager.sync()?;
    for id in retired {
        pager.free_page(id)?;
    }
    Ok(())
}

fn build_meta_page1_with_overflow(
    pager: &Pager,
    meta_data: &[u8],
) -> Result<crate::storage::engine::Page, PagerError> {
    use crate::storage::engine::{Page, PageType, HEADER_SIZE};

    let mut page1 = Page::new(PageType::Header, 1);
    let cs = HEADER_SIZE;

    if meta_data.len() <= META_PAGE_CONTENT_CAP {
        // Single-page: bit-identical to the historical layout.
        let buf = page1.as_bytes_mut();
        buf[cs..cs + meta_data.len()].copy_from_slice(meta_data);
        return Ok(page1);
    }

    // Multi-page overflow form. Split the inner payload into the first chunk
    // (held on page 1) followed by zero-or-more continuation chunks chained
    // through `PageType::Overflow` pages.
    let first_chunk = &meta_data[..META_V3_FIRST_PAYLOAD_CAP];
    let mut tail = &meta_data[META_V3_FIRST_PAYLOAD_CAP..];
    let mut chunks: Vec<&[u8]> = Vec::new();
    while !tail.is_empty() {
        let take = tail.len().min(META_V3_OVERFLOW_PAYLOAD_CAP);
        chunks.push(&tail[..take]);
        tail = &tail[take..];
    }

    let mut overflow_pages: Vec<Page> = Vec::with_capacity(chunks.len());
    let mut overflow_ids: Vec<u32> = Vec::with_capacity(chunks.len());
    for _ in 0..chunks.len() {
        let pg = pager.allocate_page(PageType::Overflow)?;
        overflow_ids.push(pg.page_id());
        overflow_pages.push(pg);
    }

    for i in 0..chunks.len() {
        let next = if i + 1 < chunks.len() {
            overflow_ids[i + 1]
        } else {
            0u32
        };
        let len = chunks[i].len() as u32;
        let buf = overflow_pages[i].as_bytes_mut();
        reddb_file::encode_native_metadata_overflow_continuation_header(
            &mut buf[cs..cs + META_V3_OVERFLOW_HEADER],
            reddb_file::NativeMetadataOverflowContinuationHeader {
                next_overflow_page_id: next,
                chunk_bytes: len,
            },
        )
        .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?;
        buf[cs + 8..cs + 8 + chunks[i].len()].copy_from_slice(chunks[i]);
    }
    for (idx, page) in overflow_pages.into_iter().enumerate() {
        let id = overflow_ids[idx];
        pager.write_page(id, page)?;
    }

    // Mirror the inner format_version for debug-friendly hex dumps.
    let format_version = reddb_file::decode_native_paged_metadata_header(meta_data)
        .ok()
        .flatten()
        .map_or(0, |header| header.format_version);

    let buf = page1.as_bytes_mut();
    reddb_file::encode_native_metadata_overflow_header(
        &mut buf[cs..cs + META_V3_PAGE1_HEADER],
        reddb_file::NativeMetadataOverflowHeader {
            format_version,
            total_payload_bytes: meta_data.len() as u32,
            next_overflow_page_id: overflow_ids[0],
        },
    )
    .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?;
    buf[cs + META_V3_PAGE1_HEADER..cs + META_V3_PAGE1_HEADER + first_chunk.len()]
        .copy_from_slice(first_chunk);

    Ok(page1)
}

fn storage_integrity(
    zone: &str,
    id: impl Into<String>,
    collection: Option<String>,
    detail: impl Into<String>,
) -> StoreError {
    StoreError::StorageIntegrity(crate::api::StorageIntegrityError::new(
        zone, id, collection, detail,
    ))
}

fn page_integrity(
    page_id: u32,
    collection: Option<String>,
    detail: impl Into<String>,
) -> StoreError {
    storage_integrity("page", page_id.to_string(), collection, detail)
}

/// Assemble the full metadata payload from the internal manifest zone: page 1
/// plus its overflow chain when the native overflow wrapper is present. Returns
/// the bytes that the metadata parser would see starting from the content
/// offset of page 1. Single-page metadata returns the raw page content
/// (including trailing zero-pad), so the legacy parser sees the same bytes it
/// always saw.
///
/// The zone is rooted by the superblock and checksummed per ADR 0074 §2: a
/// failing checksum surfaces the didactic manifest-zone integrity error
/// directly — there is no sidecar copy to fall back to since ADR 0038 §4
/// phase 1 retired the shadows.
fn read_meta_payload(pager: &Pager) -> Result<Option<Vec<u8>>, StoreError> {
    let cs = crate::storage::engine::HEADER_SIZE;
    let meta_page = pager.read_manifest_page().map_err(|err| {
        storage_integrity(
            "manifest",
            "manifest",
            None,
            format!("internal manifest zone read failed ({err})"),
        )
    })?;
    let bytes = meta_page.as_bytes();
    if bytes.len() < cs + 4 {
        return Ok(Some(bytes.get(cs..).unwrap_or(&[]).to_vec()));
    }
    let header = match reddb_file::decode_native_metadata_overflow_header(&bytes[cs..])
        .map_err(|err| page_integrity(1, None, err.to_string()))?
    {
        Some(header) => header,
        None => {
            return Ok(Some(bytes[cs..].to_vec()));
        }
    };
    if bytes.len() < cs + META_V3_PAGE1_HEADER {
        return Err(page_integrity(
            1,
            None,
            "metadata overflow header is truncated",
        ));
    }
    let total = header.total_payload_bytes as usize;
    let mut next = header.next_overflow_page_id;
    let mut payload: Vec<u8> = Vec::with_capacity(total);
    let first_take = total.min(META_V3_FIRST_PAYLOAD_CAP);
    payload.extend_from_slice(
        &bytes[cs + META_V3_PAGE1_HEADER..cs + META_V3_PAGE1_HEADER + first_take],
    );
    let mut seen = std::collections::HashSet::new();
    while next != 0 && payload.len() < total {
        if !seen.insert(next)
            || seen.len()
                >= pager
                    .page_count()
                    .map_err(|err| page_integrity(next, None, err.to_string()))?
                    as usize
        {
            return Err(page_integrity(next, None, "cyclic metadata overflow chain"));
        }
        let ov = pager
            .read_page(next)
            .map_err(|err| page_integrity(next, None, err.to_string()))?;
        if ov
            .page_type()
            .map_err(|err| page_integrity(next, None, err.to_string()))?
            != crate::storage::engine::PageType::Overflow
        {
            return Err(page_integrity(
                next,
                None,
                "invalid metadata overflow page type",
            ));
        }
        let ob = ov.as_bytes();
        if ob.len() < cs + META_V3_OVERFLOW_HEADER {
            return Err(page_integrity(
                next,
                None,
                "metadata overflow continuation is truncated",
            ));
        }
        let continuation =
            reddb_file::decode_native_metadata_overflow_continuation_header(&ob[cs..])
                .map_err(|err| page_integrity(next, None, err.to_string()))?;
        let nn = continuation.next_overflow_page_id;
        let len = continuation.chunk_bytes as usize;
        let remaining = total - payload.len();
        let take = len.min(remaining).min(META_V3_OVERFLOW_PAYLOAD_CAP);
        payload.extend_from_slice(
            &ob[cs + META_V3_OVERFLOW_HEADER..cs + META_V3_OVERFLOW_HEADER + take],
        );
        next = nn;
    }
    if payload.len() != total {
        return Err(page_integrity(
            1,
            None,
            "metadata overflow chain is incomplete",
        ));
    }
    Ok(Some(payload))
}

impl UnifiedStore {
    /// Compare and encode one borrowed row at a time. Begin a new tree only at
    /// the first difference, copying its already-verified prefix in bounded
    /// batches. Unchanged trees require no replacement allocation.
    fn checkpoint_tree(
        &self,
        pager: &Arc<Pager>,
        manager: &SegmentManager,
        previous: Option<&Arc<BTree>>,
        mut encode: impl FnMut(&UnifiedEntity, Option<Metadata>) -> Option<Vec<u8>>,
    ) -> Result<Arc<BTree>, StoreError> {
        if let Some((base, overlay)) = manager.backing_overlay(&mut encode) {
            use reddb_engine::storage::collection_read::{CollectionRead, MergedCollection};
            let merged = MergedCollection::new(base.as_ref(), overlay);
            let mut cursor = merged.cursor()?;
            let mut builder = CheckpointTreeBuilder::new(pager);
            while let Some(record) = cursor.next_record(
                usize::try_from(self.config.resident_memory_bytes_max.unwrap_or(u64::MAX))
                    .unwrap_or(usize::MAX),
            )? {
                builder.push((record.id.raw().to_be_bytes().to_vec(), record.bytes))?;
            }
            builder.flush()?;
            return Ok(builder.tree);
        }
        let mut cursor = previous
            .map(|tree| tree.cursor_first())
            .transpose()
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        let mut matched = 0;
        let mut builder = None;
        let mut failure = None;
        manager.for_each_entity_with_metadata_in_id_order(|entity, metadata| {
            let Some(value) = encode(entity, metadata) else {
                return true;
            };
            let record = (entity.id.raw().to_be_bytes().to_vec(), value);
            let result = (|| {
                if builder.is_none() {
                    if let Some(cursor) = cursor.as_mut() {
                        if cursor
                            .next()
                            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?
                            .as_ref()
                            == Some(&record)
                        {
                            matched += 1;
                            return Ok(());
                        }
                    }
                    let mut replacement = CheckpointTreeBuilder::new(pager);
                    if let Some(previous) = previous {
                        replacement.copy_prefix(previous, matched)?;
                    }
                    builder = Some(replacement);
                }
                builder
                    .as_mut()
                    .expect("replacement initialized")
                    .push(record)
            })();
            if let Err(error) = result {
                failure = Some(error);
                return false;
            }
            true
        });
        if let Some(error) = failure {
            return Err(error);
        }
        if builder.is_none() {
            if let (Some(previous), Some(cursor)) = (previous, cursor.as_mut()) {
                if cursor
                    .next()
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?
                    .is_none()
                {
                    return Ok(Arc::clone(previous));
                }
            }
            let mut replacement = CheckpointTreeBuilder::new(pager);
            if let Some(previous) = previous {
                replacement.copy_prefix(previous, matched)?;
            }
            builder = Some(replacement);
        }
        let mut builder = builder.expect("replacement or reused tree");
        builder.flush()?;
        Ok(builder.tree)
    }

    /// Publication first, reuse second. The immediately preceding generation
    /// stays in the durable ledger for one more publication. Cursors and cloned
    /// tree handles defer retirement further without holding the writer fence.
    fn publish_reclaimable_trees(
        &self,
        pager: &Pager,
        metadata: &mut Vec<u8>,
        live: &HashMap<String, Arc<BTree>>,
        keys: &HashMap<String, PublishedKeyIndex>,
    ) -> Result<(), StoreError> {
        let mut retired = self.retired_btrees.lock();
        let mut roots = Vec::new();
        let mut reclaim = Vec::new();
        let mut remove = std::collections::HashSet::new();
        for (index, entry) in retired.iter().enumerate() {
            if live
                .values()
                .chain(keys.values().map(|index| &index.tree))
                .any(|tree| Arc::ptr_eq(tree, &entry.tree))
            {
                remove.insert(index);
                continue;
            }
            if entry.published
                && Arc::strong_count(&entry.tree) == 1
                && !entry.tree.has_retirement_readers()
            {
                // Deliberately unsafe experimental control: keep the old
                // root in the catalog while releasing its pages. Never enable
                // this outside the isolated recovery test.
                #[cfg(test)]
                if std::env::var_os("REDDB_EXPERIMENTAL_RETAIN_RELEASED_ROOT").is_some() {
                    roots.push(entry.tree.root_page_id());
                }
                reclaim.extend(
                    entry
                        .tree
                        .retired_pages()
                        .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?,
                );
                remove.insert(index);
            } else if entry.tree.root_page_id() != 0 {
                roots.push(entry.tree.root_page_id());
            }
        }
        roots.sort_unstable();
        roots.dedup();
        reddb_file::encode_native_paged_retired_trees(metadata, &roots)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let (pending_pages, published_pages) = pager
            .deferred_reclamation_pages()
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        let readers = live
            .values()
            .chain(keys.values().map(|index| &index.tree))
            .any(|tree| tree.has_retirement_readers())
            || retired
                .iter()
                .any(|entry| entry.tree.has_retirement_readers());
        let released_pages = if readers { Vec::new() } else { published_pages };
        let released: std::collections::HashSet<_> = released_pages.iter().copied().collect();
        let retained_pages: Vec<_> = pending_pages
            .into_iter()
            .filter(|id| !released.contains(id))
            .collect();
        reddb_file::encode_native_paged_deferred_pages(metadata, &retained_pages)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let active_epoch = self.ownership_active_epoch.load(Ordering::Acquire);
        let catalog_epoch = if active_epoch == 0 {
            self.ownership_catalog_epoch.load(Ordering::Acquire)
        } else {
            active_epoch
        };
        if catalog_epoch != 0 {
            reddb_file::native_store::encode_native_paged_ownership_epoch(metadata, catalog_epoch)
                .map_err(|e| StoreError::Serialization(e.to_string()))?;
        }
        if self.config.resident_memory_bytes_max.is_some() {
            use reddb_file::native_resume::{
                encode_catalog_resume, CatalogResume, CollectionResume,
            };
            let mut states = std::collections::BTreeMap::new();
            let mut entity_id_next = self.resident.next_entity_id.load(Ordering::SeqCst);
            let collections = self.resident.collections.read();
            for (name, manager) in collections.iter() {
                let tree = live.get(name);
                let (entity_next, row_id_next) = manager.identity_counters();
                entity_id_next = entity_id_next.max(entity_next);
                let root_page = tree.map_or(0, |tree| tree.root_page_id());
                let entities = if root_page == 0 {
                    0
                } else if let Some(count) = manager.backing_count_at_root(root_page) {
                    count
                } else {
                    // Changed projections may exclude pending/aborted rows. Count
                    // published physical keys, not the live manager's cardinality.
                    let mut cursor = tree
                        .expect("invariant: nonzero root has a tree")
                        .cursor_first()
                        .map_err(|error| {
                            StoreError::Io(std::io::Error::other(error.to_string()))
                        })?;
                    let mut count = 0_u64;
                    while cursor
                        .skip_next()
                        .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?
                    {
                        count += 1;
                    }
                    count
                };
                states.insert(
                    name.clone(),
                    CollectionResume {
                        root_page,
                        entities,
                        entity_id_next: entity_next,
                        row_id_next,
                        sequence_id_next: manager.sequence_id_next(),
                        key_index_root: tree.and_then(|tree| {
                            keys.get(name)
                                .filter(|index| {
                                    index.indexed && index.source.ptr_eq(&Arc::downgrade(tree))
                                })
                                .map(|index| index.tree.root_page_id())
                        }),
                    },
                );
            }
            encode_catalog_resume(
                metadata,
                &CatalogResume {
                    entity_id_next,
                    collections: states,
                },
            )
            .map_err(|error| StoreError::Serialization(error.to_string()))?;
        }
        // Record the exact pages before the catalog can forget their owners.
        // Recovery resolves this intent against the durable catalog epoch.
        let mut retirement_intent = reclaim.clone();
        retirement_intent.extend_from_slice(&released_pages);
        if !retirement_intent.is_empty() {
            pager
                .record_retirement_intent(&retirement_intent)
                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        }
        // Do not mutate retirement state or the free list before both the new
        // children and the manifest that forgets old roots have been synced.
        publish_paged_metadata(pager, metadata)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        self.ownership_catalog_epoch
            .store(catalog_epoch, Ordering::Release);
        #[cfg(test)]
        if !reclaim.is_empty()
            && std::env::var_os("REDDB_EXPERIMENTAL_EXIT_BEFORE_RETIRED_FREE").is_some()
        {
            eprintln!(
                "experimental exit before retired free pages={}",
                reclaim.len()
            );
            std::process::exit(184);
        }
        let mut index = 0;
        retired.retain_mut(|entry| {
            let keep = !remove.contains(&index);
            entry.published = true;
            index += 1;
            keep
        });
        if !reclaim.is_empty() {
            pager
                .free_retired_pages(&reclaim)
                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
            #[cfg(test)]
            if std::env::var_os("REDDB_EXPERIMENTAL_EXIT_AFTER_RETIRED_ENQUEUE").is_some() {
                eprintln!(
                    "experimental exit after retired enqueue pages={}",
                    reclaim.len()
                );
                std::process::exit(186);
            }
            pager
                .sync()
                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
            #[cfg(test)]
            if std::env::var_os("REDDB_EXPERIMENTAL_EXIT_AFTER_RETIRED_FREE").is_some() {
                eprintln!(
                    "experimental exit after retired free pages={}",
                    reclaim.len()
                );
                std::process::exit(185);
            }
        }
        pager
            .complete_deferred_publication(&retained_pages, &released_pages)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        if !released_pages.is_empty() {
            pager
                .sync()
                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        }
        Ok(())
    }

    pub(super) fn retire_btree(&self, tree: Arc<BTree>) {
        let mut retired = self.retired_btrees.lock();
        if tree.root_page_id() != 0 && !retired.iter().any(|entry| Arc::ptr_eq(&entry.tree, &tree))
        {
            retired.push(RetiredBTree {
                tree,
                published: false,
            });
        }
    }

    /// A completed inline insert already made this tree match its manager.
    /// Advance the reuse proof only for that single, fenced mutation. Any
    /// untracked edit or segment publication keeps the full comparison path.
    pub(crate) fn note_paged_insert(
        &self,
        collection: &str,
        manager: &Arc<SegmentManager>,
        before: &[(u64, u64)],
        id: EntityId,
    ) {
        let after = manager.checkpoint_revisions();
        if before.len() != after.len() {
            return;
        }
        let mut changed = 0;
        for ((old_id, old_revision), (new_id, new_revision)) in before.iter().zip(&after) {
            if old_id != new_id {
                return;
            }
            if old_revision != new_revision {
                if old_revision.checked_add(1) != Some(*new_revision) {
                    return;
                }
                changed += 1;
            }
        }
        if changed != 1 {
            return;
        }
        let mut versions = self.checkpoint_versions.write();
        let Some(previous) = versions.get_mut(collection) else {
            return;
        };
        if previous.manager.ptr_eq(&Arc::downgrade(manager))
            && previous.segments == before
            && previous.format_version == self.format_version()
            && !previous.pending_vault.contains(&id)
            && !previous.excluded_xids.contains(&0)
        {
            previous.segments = after;
        }
    }

    /// Successful physical deletes already changed both the manager and tree.
    /// Reuse remains valid only when their exact revision delta accounts for
    /// this batch, with no intervening edits or segment replacement.
    pub(crate) fn note_paged_delete(
        &self,
        collection: &str,
        manager: &Arc<SegmentManager>,
        before: &[(u64, u64)],
        deleted: &[EntityId],
    ) {
        let after = manager.checkpoint_revisions();
        if before.len() != after.len() {
            return;
        }
        let mut changes = 0_u64;
        for ((old_id, old_revision), (new_id, new_revision)) in before.iter().zip(&after) {
            if old_id != new_id {
                return;
            }
            let Some(delta) = new_revision.checked_sub(*old_revision) else {
                return;
            };
            let Some(total) = changes.checked_add(delta) else {
                return;
            };
            changes = total;
        }
        let Ok(expected) = u64::try_from(deleted.len()) else {
            return;
        };
        if changes != expected {
            return;
        }
        let mut versions = self.checkpoint_versions.write();
        let Some(previous) = versions.get_mut(collection) else {
            return;
        };
        if !previous.manager.ptr_eq(&Arc::downgrade(manager))
            || previous.format_version != self.format_version()
            || deleted.iter().any(|id| previous.pending_vault.contains(id))
            || previous.excluded_xids.contains(&0)
        {
            return;
        }
        if previous.segments != before {
            let Some(deferred) = previous.deferred_rows.as_ref() else {
                return;
            };
            let Some(changed) = manager.checkpoint_mutations_since(&previous.segments) else {
                return;
            };
            let removed: std::collections::HashSet<_> = deleted.iter().copied().collect();
            let pending: std::collections::BTreeSet<_> = deferred
                .iter()
                .copied()
                .chain(changed)
                .filter(|id| !removed.contains(id))
                .collect();
            if pending.len() > crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX {
                return;
            }
            // GC already applied its deletes to the tree. Carry earlier dirty
            // IDs forward instead of spending their journal on these deletes.
            // Sparse projection still requires exact WAL proof for every
            // differing row, so an unlogged manager edit cannot be concealed.
            previous.has_unprojected_rows = !pending.is_empty();
            previous.deferred_rows = Some(Arc::new(pending.into_iter().collect()));
        }
        previous.segments = after;
    }

    pub(crate) fn mark_paged_registry_dirty(&self) {
        self.resident.mark_paged_registry_dirty()
    }

    /// Get (or lazily create) the per-collection B-tree under a *read*
    /// lock whenever possible. Returns a cloned `Arc<BTree>` so callers
    /// can mutate the tree without holding the outer map's RwLock —
    /// previously every insert serialised on `btree_indices.write()`,
    /// costing ~60% of the concurrent-insert throughput ceiling.
    pub(crate) fn get_or_create_btree(&self, collection: &str) -> Option<Arc<BTree>> {
        let pager = self.pager.as_ref()?;
        if let Some(btree) = self.btree_indices.read().get(collection).cloned() {
            return Some(btree);
        }
        let mut write = self.btree_indices.write();
        let btree = write
            .entry(collection.to_string())
            .or_insert_with(|| Arc::new(BTree::new(Arc::clone(pager))))
            .clone();
        Some(btree)
    }

    pub(crate) fn flush_paged_state(&self) -> Result<(), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok(());
        };

        if self.resident.paged_registry_dirty.load(Ordering::Acquire) {
            self.flush_paged_registry()?;
            self.resident
                .paged_registry_dirty
                .store(false, Ordering::Release);
            return Ok(());
        }

        pager
            .flush()
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))
    }

    pub(crate) fn flush_paged_registry(&self) -> Result<(), StoreError> {
        let _vault_guard = self.vault_publication_lock.lock();
        let Some(pager) = &self.pager else {
            return Ok(());
        };

        match pager.read_page(1) {
            Ok(_) => {}
            Err(PagerError::PageNotFound(_)) => {
                let meta_page = pager
                    .allocate_page(crate::storage::engine::PageType::Header)
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
                pager
                    .write_page(meta_page.page_id(), meta_page)
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
            }
            Err(e) => {
                return Err(StoreError::Io(std::io::Error::other(e.to_string())));
            }
        }

        let format_version = STORE_VERSION_V11;
        self.set_format_version(format_version);

        let collections = self.resident.collections.read();
        let btree_indices = self.btree_indices.read();
        let mut collection_roots = Vec::with_capacity(collections.len());
        for (name, _) in collections.iter() {
            let root_page = btree_indices
                .get(name)
                .map_or(0, |btree| btree.root_page_id());
            collection_roots.push((name.clone(), root_page));
        }
        drop(btree_indices);
        drop(collections);

        let mut meta_data = Vec::with_capacity(4096);
        reddb_file::encode_native_paged_metadata_header(
            &mut meta_data,
            reddb_file::NativePagedMetadataHeader {
                format_version,
                collection_count: collection_roots.len() as u32,
            },
        );
        for (name, root_page) in &collection_roots {
            reddb_file::encode_native_paged_collection_root(&mut meta_data, name, *root_page);
        }

        let cross_refs = self.resident.cross_refs.read();
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        meta_data.extend_from_slice(&(total_refs as u32).to_le_bytes());
        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                reddb_file::encode_native_paged_cross_ref(
                    &mut meta_data,
                    source_id.raw(),
                    target_id.raw(),
                    ref_type.to_byte(),
                    collection,
                );
            }
        }

        self.publish_reclaimable_trees(
            pager,
            &mut meta_data,
            &self.btree_indices.read(),
            &self.key_indices.read(),
        )?;

        Ok(())
    }

    /// Get a reference to the underlying pager (if in paged mode).
    pub fn pager(&self) -> Option<&Arc<Pager>> {
        self.pager.as_ref()
    }

    /// Bytes of page-cache memory currently occupied by resident pages
    /// (ADR 0073 §2). Slots are fixed size, so occupancy times the page size
    /// *is* the footprint; a store without a pager occupies none.
    pub fn page_cache_bytes_in_use(&self) -> u64 {
        self.pager.as_ref().map_or(0, |pager| {
            pager.cache_len() as u64 * crate::storage::memory_pools::PAGE_CACHE_PAGE_SIZE_BYTES
        })
    }

    /// Bytes of WAL buffer memory held by this store's group-commit
    /// coordinator: queued records plus the writer's fixed append buffer.
    pub fn wal_buffer_bytes_in_use(&self) -> u64 {
        self.commit
            .as_ref()
            .map_or(0, |commit| commit.buffered_bytes())
    }

    /// Approximate resident bytes across every collection's segment arena
    /// (ADR 0073 §2). Same number `stats().total_memory_bytes` reports, without
    /// building the per-collection stats map a sampler would throw away.
    pub fn segment_memory_bytes(&self) -> u64 {
        let managers: Vec<Arc<SegmentManager>> = self
            .resident
            .collections
            .read()
            .values()
            .map(Arc::clone)
            .collect();

        managers
            .iter()
            .map(|manager| manager.resident_bytes())
            .fold(0, u64::saturating_add)
    }

    /// Borrow the immutable store configuration. Runtime hooks (e.g. the
    /// `auto_index_id` first-insert hook in `MutationEngine`) read knobs
    /// off this struct without going through the legacy global config tree.
    pub fn config(&self) -> &UnifiedStoreConfig {
        &self.config
    }

    pub fn with_config(config: UnifiedStoreConfig) -> Self {
        Self {
            config,
            resident: reddb_engine::storage::resident_store::ResidentStore::new(),
            format_version: AtomicU32::new(STORE_VERSION_V11),
            pager: None,
            db_path: None,
            btree_indices: RwLock::new(HashMap::new()),
            key_indices: RwLock::new(HashMap::new()),
            checkpoint_versions: RwLock::new(HashMap::new()),
            checkpoint_durable_rows: RwLock::new(super::checkpoint_delta::DurableRows::default()),
            retired_btrees: parking_lot::Mutex::new(Vec::new()),
            ownership_catalog_epoch: AtomicU64::new(0),
            ownership_active_epoch: AtomicU64::new(0),
            detached_native_blob_pages: parking_lot::Mutex::new(Default::default()),
            commit: None,
            replayed_turbo_inserts: parking_lot::Mutex::new(HashMap::new()),
            replayed_probabilistic_deltas: parking_lot::Mutex::new(Vec::new()),
            snapshot_manager: std::sync::OnceLock::new(),
            embedded_commit_session: std::sync::OnceLock::new(),
            #[cfg(test)]
            test_fail_deferred_wal_append: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            test_require_deferred_publication_fence: std::sync::atomic::AtomicBool::new(false),
            vault_publication_lock: parking_lot::ReentrantMutex::new(()),
            pending_vault_versions: RwLock::new(std::collections::HashSet::new()),
        }
    }

    /// Open or create a page-based database
    ///
    /// This uses the page engine for ACID durability with B-tree indices.
    /// The database file uses 16 KiB pages with checksums and efficient caching.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the database file (e.g., "data.rdb")
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let store = UnifiedStore::open("security.rdb")?;
    /// store.create_collection("hosts")?;
    /// // ... operations ...
    /// store.persist()?; // Flush to disk
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with_config(path, UnifiedStoreConfig::default())
    }

    pub fn open_with_config(
        path: impl AsRef<Path>,
        config: UnifiedStoreConfig,
    ) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let mut pager_config = PagerConfig {
            disk_residency: config.resident_memory_bytes_max.is_some(),
            ..PagerConfig::default()
        };
        // ADR 0073 §2 — the page cache is pre-sized from its budget share.
        // The pager's own `cache_size` default survives only for direct
        // library callers who never resolved a budget.
        if let Some(slots) = config.page_cache_slots {
            pager_config.cache_size = slots;
        }
        // Tunables via env — experimental, used by the benchmark harness
        // to compare durability profiles head-to-head with Postgres.
        // REDDB_DOUBLE_WRITE=0 requests skipping the double-write buffer,
        // which otherwise adds two fsyncs per pager flush (one on DWB, one
        // on the main file). The pager honors this only when the actual
        // data file is proven to live on a CoW filesystem with atomic page
        // writes; otherwise it fails closed and keeps DWB enabled.
        if matches!(
            std::env::var("REDDB_DOUBLE_WRITE").ok().as_deref(),
            Some("0") | Some("false") | Some("off")
        ) {
            pager_config.double_write = false;
        }
        let pager = Pager::open(path, pager_config)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        pager
            .enable_deferred_reclamation()
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;

        if config.resident_memory_bytes_max.is_none()
            && pager
                .header()
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?
                .version
                >= reddb_file::file_format::PAGE_DISK_RESIDENCY_FILE_VERSION
        {
            return Err(StoreError::Io(std::io::Error::new(std::io::ErrorKind::Unsupported,
                "this database was prepared for disk residency; reopen with the disk-resident storage profile")));
        }

        let wal_path = reddb_file::layout::unified_wal_path(path);
        // Explicit transactions and WAL-only vault versions need committed
        // batches in every durability mode, including Strict's inline fsync.
        let commit = Some(Arc::new(
            StoreCommitCoordinator::open(wal_path, config.durability_mode, config.group_commit)
                .map_err(StoreError::Io)?,
        ));

        let store = Self {
            config,
            resident: reddb_engine::storage::resident_store::ResidentStore::new(),
            format_version: AtomicU32::new(STORE_VERSION_V11),
            pager: Some(Arc::new(pager)),
            db_path: Some(path.to_path_buf()),
            btree_indices: RwLock::new(HashMap::new()),
            key_indices: RwLock::new(HashMap::new()),
            checkpoint_versions: RwLock::new(HashMap::new()),
            checkpoint_durable_rows: RwLock::new(super::checkpoint_delta::DurableRows::default()),
            retired_btrees: parking_lot::Mutex::new(Vec::new()),
            ownership_catalog_epoch: AtomicU64::new(0),
            ownership_active_epoch: AtomicU64::new(0),
            detached_native_blob_pages: parking_lot::Mutex::new(Default::default()),
            commit,
            replayed_turbo_inserts: parking_lot::Mutex::new(HashMap::new()),
            replayed_probabilistic_deltas: parking_lot::Mutex::new(Vec::new()),
            snapshot_manager: std::sync::OnceLock::new(),
            embedded_commit_session: std::sync::OnceLock::new(),
            #[cfg(test)]
            test_fail_deferred_wal_append: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            test_require_deferred_publication_fence: std::sync::atomic::AtomicBool::new(false),
            vault_publication_lock: parking_lot::ReentrantMutex::new(()),
            pending_vault_versions: RwLock::new(std::collections::HashSet::new()),
        };

        // An interrupted structural edit may have reached home pages without
        // publishing its catalog. Restore those pages before following roots.
        if let Some(commit) = &store.commit {
            commit
                .restore_checkpoint_page_images(&store)
                .map_err(StoreError::Io)?;
        }
        // Load existing data from pages if database exists
        store.load_from_pages()?;
        if let Some(commit) = &store.commit {
            commit.replay_into(&store).map_err(StoreError::Io)?;
        }
        store.recover_operational_manifest()?;
        if store.config.resident_memory_bytes_max.is_some() {
            let missing_key_index = {
                let trees = store.btree_indices.read();
                let keys = store.key_indices.read();
                trees.iter().any(|(name, tree)| {
                    !keys
                        .get(name)
                        .is_some_and(|index| index.source.ptr_eq(&Arc::downgrade(tree)))
                })
            };
            if missing_key_index {
                store.persist()?;
            }
        }

        Ok(store)
    }

    /// Replay changes the authoritative managers first. Build replacement trees
    /// only for changed collections, leaving every published tree untouched until
    /// all replacement pages are durable. Retain the WAL for auxiliary recovery.
    pub(crate) fn reconcile_replayed_btrees(&self) -> Result<(), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok(());
        };
        let collections = self.resident.collections.read();
        let mut indices = self.btree_indices.write();
        let previous_versions = self.checkpoint_versions.read();
        let raw_replay =
            self.snapshot_manager.get().is_none() && self.pending_vault_versions.read().is_empty();
        for (name, manager) in collections.iter() {
            let mut reusable = None;
            if raw_replay {
                if let (Some(previous), Some(tree)) =
                    (previous_versions.get(name), indices.get(name))
                {
                    let mut current = CheckpointVersion {
                        manager: Arc::downgrade(manager),
                        segments: manager.checkpoint_revisions(),
                        visibility_revision: 0,
                        untracked_visibility_revision: 0,
                        excluded_xids: Arc::default(),
                        pending_vault: Arc::default(),
                        format_version: self.format_version(),
                        deferred_rows: Some(Arc::default()),
                        has_unprojected_rows: false,
                    };
                    if previous.manager.ptr_eq(&current.manager)
                        && previous.format_version == current.format_version
                    {
                        if previous.segments == current.segments
                            && !previous.has_unprojected_rows
                            && checkpoint_visibility_matches(previous, &current)
                        {
                            reusable = Some(Arc::clone(tree));
                        } else {
                            // The replay ownership epoch protects in-place
                            // changes with the same undo/publication protocol
                            // used by normal sparse checkpoints. Missing WAL
                            // proof, old formats and exhausted journals retain
                            // the full comparison/rebuild path below.
                            reusable = self.sparse_checkpoint_tree(
                                name,
                                manager,
                                tree,
                                previous,
                                &mut current,
                            )?;
                        }
                    }
                }
            }
            let tree = match reusable {
                Some(tree) => tree,
                None => {
                    self.checkpoint_tree(pager, manager, indices.get(name), |entity, metadata| {
                        #[cfg(test)]
                        REPLAY_SERIALIZED_ROWS.with(|count| count.set(count.get() + 1));
                        Some(Self::serialize_entity_record(
                            entity,
                            metadata.as_ref(),
                            self.format_version(),
                        ))
                    })?
                }
            };
            if indices
                .get(name)
                .is_some_and(|previous| Arc::ptr_eq(previous, &tree))
            {
                continue;
            }
            if let Some(previous) = indices.insert(name.clone(), tree) {
                self.retire_btree(previous);
            }
            self.mark_paged_registry_dirty();
            #[cfg(test)]
            if std::env::var_os("REDDB_EXPERIMENTAL_EXIT_DURING_REPLAY_REBUILD").is_some() {
                if std::env::var_os("REDDB_EXPERIMENTAL_REPLAY_NO_SYNC").is_none() {
                    pager.sync().map_err(|error| {
                        StoreError::Io(std::io::Error::other(error.to_string()))
                    })?;
                }
                eprintln!("experimental replay rebuild exit before manifest");
                std::process::exit(182);
            }
        }
        drop(previous_versions);
        drop(indices);
        drop(collections);
        self.flush_paged_state()?;
        #[cfg(test)]
        if std::env::var_os("REDDB_EXPERIMENTAL_EXIT_AFTER_REPLAY_PUBLICATION").is_some() {
            eprintln!("experimental replay exit after manifest");
            std::process::exit(182);
        }
        // Opening has no concurrent writers. Replay has now durably reconciled
        // these raw trees; do not immediately rebuild them at the first flush.
        if self.snapshot_manager.get().is_none() && self.pending_vault_versions.read().is_empty() {
            let collections = self.resident.collections.read();
            let mut versions = self.checkpoint_versions.write();
            for (name, manager) in collections.iter() {
                versions.insert(
                    name.clone(),
                    CheckpointVersion {
                        manager: Arc::downgrade(manager),
                        segments: manager.checkpoint_revisions(),
                        visibility_revision: 0,
                        untracked_visibility_revision: 0,
                        excluded_xids: Arc::default(),
                        pending_vault: Arc::default(),
                        format_version: self.format_version(),
                        deferred_rows: Some(Arc::default()),
                        has_unprojected_rows: false,
                    },
                );
            }
        }
        Ok(())
    }

    pub(crate) fn recover_operational_manifest(&self) -> Result<(), StoreError> {
        let Some(path) = &self.db_path else {
            return Ok(());
        };
        let mut collections = self.list_collections();
        collections.sort();
        let pending_drops =
            crate::storage::operational_manifest::OperationalManifest::for_db_path(path)
                .recover_or_bootstrap(&collections)
                .map_err(|err| storage_integrity("manifest", "current", None, err.to_string()))?;
        for name in pending_drops {
            if self.get_collection(&name).is_some() {
                self.drop_collection(&name)?;
            }
        }
        Ok(())
    }

    pub(crate) fn prepare_operational_collection_create(
        &self,
        name: &str,
    ) -> Result<Option<reddb_file::PreparedCollectionCreate<reddb_file::StdVfs>>, StoreError> {
        let Some(path) = &self.db_path else {
            return Ok(None);
        };
        crate::storage::operational_manifest::OperationalManifest::for_db_path(path)
            .prepare_collection_create(name)
            .map(Some)
            .map_err(StoreError::Io)
    }

    pub(crate) fn prepare_operational_collection_drop(
        &self,
        name: &str,
    ) -> Result<Option<reddb_file::PreparedCollectionDrop<reddb_file::StdVfs>>, StoreError> {
        let Some(path) = &self.db_path else {
            return Ok(None);
        };
        crate::storage::operational_manifest::OperationalManifest::for_db_path(path)
            .prepare_collection_drop(name)
            .map(Some)
            .map_err(StoreError::Io)
    }

    /// Read the commit boundary before following potentially interrupted trees.
    pub(super) fn published_ownership_epoch(&self) -> Result<u64, StoreError> {
        let Some(pager) = &self.pager else {
            return Ok(0);
        };
        let Some(content) = read_meta_payload(pager)? else {
            return Ok(0);
        };
        reddb_file::native_store::decode_native_paged_catalog_ownership_epoch(&content)
            .map(|epoch| epoch.unwrap_or(0))
            .map_err(|error| StoreError::Serialization(error.to_string()))
    }

    /// Read the B-tree indices and reconstruct collections from pages.
    fn load_from_pages(&self) -> Result<(), StoreError> {
        let pager = match &self.pager {
            Some(p) => p,
            None => return Ok(()), // No pager, nothing to load
        };

        // Get page count
        let page_count = pager.page_count().map_err(|e| {
            StoreError::Io(std::io::Error::other(format!(
                "failed to read page count: {}",
                e
            )))
        })?;
        if page_count <= 1 {
            // Empty database (only header page)
            return Ok(());
        }

        // Read metadata starting from page 1 (collections registry). The
        // helper transparently follows the `RDM3` overflow chain when the
        // metadata blob spans multiple pages and falls back to the legacy
        // `<data>-meta` shadow when page 1 itself is corrupted.
        if let Some(content_vec) = read_meta_payload(pager)? {
            let content: &[u8] = &content_vec;
            let prepared = pager
                .header()
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?
                .version
                >= reddb_file::file_format::PAGE_DISK_RESIDENCY_FILE_VERSION;
            let resume = if prepared {
                reddb_file::native_resume::decode_catalog_resume(content)
                    .map_err(|error| StoreError::Serialization(error.to_string()))?
            } else {
                None
            };
            if let Some(resume) = &resume {
                self.resident
                    .next_entity_id
                    .fetch_max(resume.entity_id_next, Ordering::SeqCst);
            }
            if content.len() >= 4 {
                let mut pos = 0;
                let mut format_version = STORE_VERSION_V1;

                let collection_count = if let Some(header) =
                    reddb_file::decode_native_paged_metadata_header(content)
                        .map_err(|err| StoreError::Serialization(err.to_string()))?
                {
                    format_version = header.format_version;
                    pos += reddb_file::METADATA_HEADER_BYTES;
                    header.collection_count as usize
                } else {
                    let count = u32::from_le_bytes([
                        content[pos],
                        content[pos + 1],
                        content[pos + 2],
                        content[pos + 3],
                    ]) as usize;
                    pos += 4;
                    count
                };

                self.set_format_version(format_version);

                if pos > content.len() {
                    return Ok(());
                }

                // Read collection names and their B-tree root pages
                for _ in 0..collection_count {
                    let root = reddb_file::decode_native_paged_collection_root(content, &mut pos)
                        .map_err(|err| page_integrity(1, None, err.to_string()))?;
                    // Root page ID for this collection's B-tree
                    let root_page = root.root_page;
                    let name = root.collection;

                    // Hydrate the collection in memory only. Loading must
                    // not emit WAL entries or rewrite the on-disk registry
                    // before the existing B-tree roots are attached.
                    let _ = self.create_collection_in_memory(&name);

                    let counters = resume
                        .as_ref()
                        .and_then(|resume| resume.collections.get(&name));
                    if let (Some(counters), Some(manager)) = (counters, self.get_collection(&name))
                    {
                        manager.restore_identity_counters(
                            counters.entity_id_next,
                            counters.row_id_next,
                        );
                        manager.restore_sequence_id_next(counters.sequence_id_next);
                    }
                    // Load B-tree with root page if it exists
                    if root_page > 0 {
                        let btree = BTree::with_root(Arc::clone(pager), root_page);

                        // Load all entities from B-tree into the collection
                        let mut cursor = btree.cursor_first().map_err(|err| {
                            page_integrity(root_page, Some(name.clone()), err.to_string())
                        })?;
                        let manager = self.get_collection(&name);
                        let mut persisted_entities = counters.map_or(0, |state| state.entities);
                        while counters.is_none() {
                            let next = cursor
                                .next_with_budget(
                                    self.config
                                        .resident_memory_bytes_max
                                        .and_then(|limit| usize::try_from(limit).ok())
                                        .unwrap_or(usize::MAX),
                                )
                                .map_err(|err| {
                                    page_integrity(root_page, Some(name.clone()), err.to_string())
                                })?;
                            let Some((key, value)) = next else {
                                break;
                            };
                            persisted_entities += 1;
                            // Deserialize entity from value bytes
                            let key_bytes: [u8; 8] = key.as_slice().try_into().map_err(|_| {
                                page_integrity(
                                    root_page,
                                    Some(name.clone()),
                                    "physical entity key must contain eight bytes",
                                )
                            })?;
                            let record = reddb_engine::storage::collection_read::EncodedRecord {
                                id: EntityId::new(u64::from_be_bytes(key_bytes)),
                                bytes: value,
                            };
                            let decoded = if let Some(limit) = self.config.resident_memory_bytes_max
                            {
                                record.decode_with_budget(
                                    self.format_version(),
                                    usize::try_from(limit).unwrap_or(usize::MAX),
                                )
                            } else {
                                record.decode(self.format_version())
                            };
                            let (entity, metadata) = decoded.map_err(|err| {
                                page_integrity(root_page, Some(name.clone()), err.to_string())
                            })?;
                            if let Some(m) = &manager {
                                let id = entity.id;
                                if let EntityKind::TableRow { row_id, .. } = &entity.kind {
                                    m.register_row_id(*row_id);
                                }
                                if self.config.resident_memory_bytes_max.is_some() {
                                    self.register_entity_id(id);
                                    m.restore_sequence_id_next(
                                        entity.sequence_id.saturating_add(1),
                                    );
                                    m.restore_identity_counters(
                                        id.raw().saturating_add(1),
                                        m.identity_counters().1,
                                    );
                                    continue;
                                }
                                self.resident.context_index.index_entity(&name, &entity);
                                m.restore_snapshot_entity(entity.clone())?;
                                if let Some(metadata) = metadata {
                                    let _ = m.set_metadata(id, metadata);
                                }
                                self.register_entity_id(id);
                                if self.config.auto_index_refs {
                                    self.index_cross_refs(&entity, &name)?;
                                }
                            }
                        }

                        // Store the B-tree for future lookups
                        let btree = Arc::new(btree);
                        let key_tree = counters
                            .and_then(|state| state.key_index_root)
                            .map(|root| Arc::new(BTree::with_root(Arc::clone(pager), root)));
                        if counters.is_some() {
                            self.key_indices.write().insert(
                                name.clone(),
                                PublishedKeyIndex {
                                    indexed: key_tree.is_some(),
                                    source: Arc::downgrade(&btree),
                                    tree: key_tree.clone().unwrap_or_else(|| {
                                        Arc::new(BTree::with_root(Arc::clone(pager), 0))
                                    }),
                                },
                            );
                        }
                        if let (Some(manager), Some(limit)) =
                            (&manager, self.config.resident_memory_bytes_max)
                        {
                            manager.install_backing(
                                Arc::new(
                                    match key_tree {
                                        Some(keys) => reddb_engine::storage::collection_read::PagedCollection::with_key_index(Arc::clone(&btree), keys),
                                        None => reddb_engine::storage::collection_read::PagedCollection::new(Arc::clone(&btree)),
                                    },
                                ),
                                btree.root_page_id(),
                                format_version,
                                usize::try_from(limit).unwrap_or(usize::MAX),
                                persisted_entities,
                            );
                        }
                        self.btree_indices.write().insert(name.clone(), btree);
                        if format_version == STORE_VERSION_V11 {
                            if let Some(manager) = &manager {
                                self.checkpoint_versions.write().insert(
                                    name,
                                    CheckpointVersion {
                                        manager: Arc::downgrade(manager),
                                        segments: manager.checkpoint_revisions(),
                                        visibility_revision: 0,
                                        untracked_visibility_revision: 0,
                                        excluded_xids: Arc::default(),
                                        pending_vault: Arc::default(),
                                        format_version,
                                        deferred_rows: Some(Arc::default()),
                                        has_unprojected_rows: false,
                                    },
                                );
                            }
                        }
                    }
                }

                if format_version >= STORE_VERSION_V2 && pos + 4 <= content.len() {
                    let cross_ref_count = u32::from_le_bytes([
                        content[pos],
                        content[pos + 1],
                        content[pos + 2],
                        content[pos + 3],
                    ]) as usize;
                    pos += 4;

                    for _ in 0..cross_ref_count {
                        let Ok(cross_ref) =
                            reddb_file::decode_native_paged_cross_ref(content, &mut pos)
                        else {
                            break;
                        };
                        let source_id = EntityId::new(cross_ref.source_id);
                        let target_id = EntityId::new(cross_ref.target_id);
                        let ref_type = RefType::from_byte(cross_ref.ref_type);
                        let target_collection = cross_ref.target_collection;

                        self.resident
                            .cross_refs
                            .write()
                            .entry(source_id)
                            .or_default()
                            .push((target_id, ref_type, target_collection.clone()));

                        if let Some((collection, mut entity)) = self.get_any(source_id) {
                            let exists = entity.cross_refs().iter().any(|xref| {
                                xref.target == target_id
                                    && xref.ref_type == ref_type
                                    && xref.target_collection == target_collection
                            });
                            if !exists {
                                entity.cross_refs_mut().push(CrossRef::new(
                                    source_id,
                                    target_id,
                                    target_collection.clone(),
                                    ref_type,
                                ));
                                if let Some(manager) = self.get_collection(&collection) {
                                    let _ = manager.update(entity);
                                }
                            }
                        }
                    }
                }
                let retired = reddb_file::decode_native_paged_retired_trees(content, &mut pos)
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;
                let live_roots: std::collections::HashSet<_> = self
                    .btree_indices
                    .read()
                    .values()
                    .map(|tree| tree.root_page_id())
                    .collect();
                for root in retired {
                    if live_roots.contains(&root)
                        || root
                            >= pager
                                .page_count()
                                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?
                    {
                        return Err(page_integrity(
                            root,
                            None,
                            "retired root aliases a live or missing page",
                        ));
                    }
                    self.retired_btrees.lock().push(RetiredBTree {
                        tree: Arc::new(BTree::with_root(Arc::clone(pager), root)),
                        published: true,
                    });
                }
                let deferred = reddb_file::decode_native_paged_deferred_pages(content, &mut pos)
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;
                if deferred.iter().any(|id| live_roots.contains(id)) {
                    return Err(page_integrity(1, None, "deferred page aliases a live root"));
                }
                pager
                    .restore_deferred_reclamation(&deferred)
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
                let ownership_epoch =
                    reddb_file::native_store::decode_native_paged_ownership_epoch(
                        content, &mut pos,
                    )
                    .map_err(|e| StoreError::Serialization(e.to_string()))?;
                self.ownership_catalog_epoch
                    .store(ownership_epoch.unwrap_or(0), Ordering::Release);
            }
        }

        if self.format_version() < STORE_VERSION_V11 {
            self.set_format_version(STORE_VERSION_V11);
        }

        Ok(())
    }

    /// Deserialize an entity from binary bytes
    pub(crate) fn deserialize_entity(
        data: &[u8],
        format_version: u32,
    ) -> Result<UnifiedEntity, StoreError> {
        let mut pos = 0;
        Self::read_entity_binary(data, &mut pos, format_version)
            .map_err(|e| StoreError::Serialization(e.to_string()))
    }

    /// Serialize an entity to binary bytes
    pub(crate) fn serialize_entity(entity: &UnifiedEntity, format_version: u32) -> Vec<u8> {
        // Pre-allocate ~256 bytes to cover the typical 15-column
        // typed row without any Vec growth. Bulk insert calls this
        // millions of times per bench run; saving 2-3 reallocs per
        // entity amortises.
        let mut buf = Vec::with_capacity(256);
        Self::write_entity_binary(&mut buf, entity, format_version);
        buf
    }

    pub(crate) fn serialize_entity_record(
        entity: &UnifiedEntity,
        metadata: Option<&Metadata>,
        format_version: u32,
    ) -> Vec<u8> {
        reddb_engine::storage::metadata_codec::serialize_entity_record(
            entity,
            metadata,
            format_version,
        )
    }

    pub(crate) fn deserialize_entity_record(
        data: &[u8],
        format_version: u32,
    ) -> Result<(UnifiedEntity, Option<Metadata>), StoreError> {
        reddb_engine::storage::metadata_codec::deserialize_entity_record(data, format_version)
            .map_err(StoreError::Serialization)
    }

    /// Persist all data to page-based storage
    ///
    /// Writes all entities to B-tree pages and flushes to disk.
    /// This provides ACID durability guarantees.
    pub fn persist(&self) -> Result<(), StoreError> {
        let _vault_guard = self.vault_publication_lock.lock();
        let pending_vault_versions = self.pending_vault_versions.read();
        let pager = match &self.pager {
            Some(p) => p,
            None => {
                // No pager attached - use binary file fallback if path available
                if let Some(path) = &self.db_path {
                    return self
                        .save_to_file(path)
                        .map_err(|e| StoreError::Serialization(e.to_string()));
                }
                return Err(StoreError::Io(std::io::Error::other(
                    "No pager or path configured for persistence",
                )));
            }
        };
        // Prevent unrelated allocations or frees from being attributed to
        // this checkpoint's ownership epoch.
        let _ownership_guard = pager.lock_ownership_mutations();

        match pager.read_page(1) {
            Ok(_) => {}
            Err(PagerError::PageNotFound(_)) => {
                let meta_page = pager
                    .allocate_page(crate::storage::engine::PageType::Header)
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
                pager
                    .write_page(meta_page.page_id(), meta_page)
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
            }
            Err(e) => {
                return Err(StoreError::Io(std::io::Error::other(e.to_string())));
            }
        }

        if let Some(commit) = &self.commit {
            commit.force_sync().map_err(StoreError::Io)?;
        }

        struct InterruptedOwnership<'a> {
            pager: &'a Pager,
            active: bool,
        }
        impl Drop for InterruptedOwnership<'_> {
            fn drop(&mut self) {
                if self.active {
                    self.pager.fence_interrupted_ownership();
                }
            }
        }
        let mut ownership = InterruptedOwnership {
            pager,
            active: self.commit.is_some(),
        };
        let epoch = if let Some(commit) = &self.commit {
            let epoch = commit
                .next_page_ownership_epoch(self)
                .map_err(StoreError::Io)?;
            commit
                .begin_page_ownership_epoch(self, epoch)
                .map_err(StoreError::Io)?;
            Some(epoch)
        } else {
            None
        };

        let collections = self.resident.collections.read();
        let mut btree_indices = self.btree_indices.write();

        // Collect collection names and their B-tree root pages
        let mut collection_roots: Vec<(String, u32)> = Vec::new();

        // Build unpublished replacements. Deleting from the current trees here
        // would destroy the last checkpoint if a write fails before publication.
        let mut rebuilt_indices = HashMap::with_capacity(collections.len());
        let previous_versions = self.checkpoint_versions.read();
        let mut checkpoint_versions = HashMap::with_capacity(collections.len());
        let pending_vault = Arc::new(pending_vault_versions.clone());
        let (visibility_revision, excluded, untracked_visibility_revision) =
            self.snapshot_manager.get().map_or_else(
                || (0, Default::default(), 0),
                |manager| manager.checkpoint_projection_state(),
            );
        let excluded = Arc::new(excluded);
        for (name, manager) in collections.iter() {
            let mut version = CheckpointVersion {
                manager: Arc::downgrade(manager),
                segments: manager.checkpoint_revisions(),
                visibility_revision,
                untracked_visibility_revision,
                excluded_xids: Arc::clone(&excluded),
                pending_vault: Arc::clone(&pending_vault),
                format_version: self.format_version(),
                deferred_rows: Some(Arc::default()),
                has_unprojected_rows: false,
            };
            if let (Some(previous), Some(tree)) =
                (previous_versions.get(name), btree_indices.get(name))
            {
                if previous.manager.ptr_eq(&version.manager)
                    && previous.segments == version.segments
                    && previous.format_version == version.format_version
                    && !previous.has_unprojected_rows
                    && checkpoint_visibility_matches(previous, &version)
                {
                    version.deferred_rows = previous.deferred_rows.clone();
                    collection_roots.push((name.clone(), tree.root_page_id()));
                    rebuilt_indices.insert(name.clone(), Arc::clone(tree));
                    checkpoint_versions.insert(name.clone(), version);
                    continue;
                }
            }
            if let (Some(previous), Some(tree)) =
                (previous_versions.get(name), btree_indices.get(name))
            {
                if previous.manager.ptr_eq(&version.manager)
                    && previous.format_version == version.format_version
                {
                    if let Some(tree) =
                        self.sparse_checkpoint_tree(name, manager, tree, previous, &mut version)?
                    {
                        collection_roots.push((name.clone(), tree.root_page_id()));
                        rebuilt_indices.insert(name.clone(), tree);
                        checkpoint_versions.insert(name.clone(), version);
                        continue;
                    }
                }
            }
            let btree = self.checkpoint_tree(
                pager,
                manager,
                btree_indices.get(name),
                |entity, metadata| {
                    #[cfg(test)]
                    CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(count.get() + 1));
                    let hidden = pending_vault_versions.contains(&entity.id)
                        || (matches!(
                            entity.data,
                            EntityData::Row(_)
                                | EntityData::Vector(_)
                                | EntityData::QueueMessage(_)
                        ) && excluded.contains(&entity.xmin));
                    let normalize_xmax = matches!(
                        entity.data,
                        EntityData::Row(_) | EntityData::QueueMessage(_)
                    ) && excluded.contains(&entity.xmax);
                    if hidden || normalize_xmax {
                        version.remember_deferred_row(entity.id);
                    }
                    if hidden {
                        return None;
                    }
                    let mut normalized = std::borrow::Cow::Borrowed(entity);
                    if normalize_xmax {
                        normalized.to_mut().xmax = 0;
                    }
                    Some(Self::serialize_entity_record(
                        &normalized,
                        metadata.as_ref(),
                        self.format_version(),
                    ))
                },
            )?;
            checkpoint_versions.insert(name.clone(), version);

            collection_roots.push((name.clone(), btree.root_page_id()));
            rebuilt_indices.insert(name.clone(), btree);
        }

        // Write collection metadata to page 1
        let mut meta_data = Vec::with_capacity(4096);

        let format_version = STORE_VERSION_V11;
        self.set_format_version(format_version);

        reddb_file::encode_native_paged_metadata_header(
            &mut meta_data,
            reddb_file::NativePagedMetadataHeader {
                format_version,
                collection_count: collection_roots.len() as u32,
            },
        );

        // Write each collection's name and B-tree root page
        for (name, root_page) in &collection_roots {
            reddb_file::encode_native_paged_collection_root(&mut meta_data, name, *root_page);
        }

        // Write cross-reference metadata
        let cross_refs = self.resident.cross_refs.read();
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        meta_data.extend_from_slice(&(total_refs as u32).to_le_bytes());
        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                reddb_file::encode_native_paged_cross_ref(
                    &mut meta_data,
                    source_id.raw(),
                    target_id.raw(),
                    ref_type.to_byte(),
                    collection,
                );
            }
        }

        for (name, previous) in btree_indices.iter() {
            if !rebuilt_indices
                .get(name)
                .is_some_and(|tree| Arc::ptr_eq(tree, previous))
            {
                self.retire_btree(Arc::clone(previous));
            }
        }
        #[cfg(test)]
        if std::env::var_os("REDDB_EXPERIMENTAL_EXIT_DURING_CHECKPOINT_REBUILD").is_some() {
            pager
                .sync()
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
            eprintln!("experimental checkpoint exit before manifest");
            std::process::exit(183);
        }
        let rebuilt_keys = self.checkpoint_key_indices(pager, &rebuilt_indices)?;
        self.publish_reclaimable_trees(pager, &mut meta_data, &rebuilt_indices, &rebuilt_keys)?;

        #[cfg(test)]
        if std::env::var_os("REDDB_TEST_EXIT_AFTER_UNDO_CATALOG_PUBLICATION").is_some() {
            std::process::exit(195);
        }

        // Only the now-durable catalog may replace the live tree handles. The
        // previous generation is retained in the manifest retirement ledger.
        *btree_indices = rebuilt_indices;
        *self.key_indices.write() = rebuilt_keys;
        drop(previous_versions);
        *self.checkpoint_versions.write() = checkpoint_versions;

        if let Some(commit) = &self.commit {
            commit
                .finish_page_ownership_epoch(self, epoch.expect("WAL ownership epoch"))
                .map_err(StoreError::Io)?;
            ownership.active = false;
            commit.truncate().map_err(StoreError::Io)?;
        }
        *self.checkpoint_durable_rows.write() = Default::default();

        if let Some(limit) = self.config.resident_memory_bytes_max {
            let mut versions = self.checkpoint_versions.write();
            for (name, manager) in collections.iter() {
                if versions
                    .get(name)
                    .is_some_and(|version| !version.has_unprojected_rows)
                {
                    if let Some(tree) = btree_indices.get(name) {
                        manager.install_backing(
                            Arc::new(
                                {
                                    let keys = self.key_indices.read();
                                    let index = keys.get(name).expect("invariant: prepared checkpoint classifies its key index");
                                    if index.indexed {
                                        reddb_engine::storage::collection_read::PagedCollection::with_key_index(Arc::clone(tree), Arc::clone(&index.tree))
                                    } else {
                                        reddb_engine::storage::collection_read::PagedCollection::new(Arc::clone(tree))
                                    }
                                },
                            ),
                            tree.root_page_id(),
                            self.format_version(),
                            usize::try_from(limit).unwrap_or(usize::MAX),
                            manager.count() as u64,
                        );
                        if let Some(version) = versions.get_mut(name) {
                            version.segments = manager.checkpoint_revisions();
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if the store is using page-based persistence
    pub fn is_paged(&self) -> bool {
        self.pager.is_some()
    }

    /// Current root page for a collection's primary B-tree, if one has
    /// been materialized in this store.
    pub(crate) fn collection_root_page(&self, collection: &str) -> Option<u32> {
        self.btree_indices
            .read()
            .get(collection)
            .map(|btree| btree.root_page_id())
            .filter(|root| *root != 0)
    }

    /// Get the database file path (if using paged mode)
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }
}

#[cfg(test)]
mod checkpoint_work_tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn interrupted_inline_sparse_checkpoint_replays_durable_rows() {
        const CHILD: &str = "REDDB_TEST_INLINE_SPARSE_PATH";
        if let Some(path) = std::env::var_os(CHILD) {
            let insert = std::env::var_os("REDDB_TEST_INLINE_SPARSE_INSERT").is_some();
            let multi = std::env::var_os("REDDB_TEST_INLINE_SPARSE_MULTI").is_some();
            let store = UnifiedStore::open(path).expect("open store");
            let initial_count = if multi { 600 } else { 1 };
            for id in 1..=initial_count {
                store.insert_auto("rows", row(id, 1)).expect("seed");
            }
            store.persist().expect("baseline");
            let manager = store.get_collection("rows").expect("collection");
            let ids: Vec<u64> = if multi {
                (1..=initial_count).collect()
            } else {
                vec![if insert { 2 } else { 1 }]
            };
            let mut actions = Vec::new();
            for id in ids {
                if insert {
                    manager.insert(row(id, 9)).expect("insert version");
                } else {
                    manager.update(row(id, 9)).expect("update version");
                }
                let entity = manager.get(EntityId::new(id)).expect("changed entity");
                actions.push(StoreWalAction::upsert_entity(
                    "rows",
                    &entity,
                    None,
                    store.format_version(),
                ));
            }
            let references = store
                .commit
                .as_ref()
                .expect("coordinator")
                .append_checkpoint_actions(&actions)
                .expect("durable WAL");
            store.remember_checkpoint_rows(actions, references);
            store.persist().expect("exit after sparse write");
            panic!("inline sparse crash hook was not reached");
        }
        for (insert, multi) in [(false, false), (true, false), (false, true)] {
            let directory = tempfile::tempdir().expect("temporary store");
            let path = directory.path().join("data.rdb");
            let mut command =
                std::process::Command::new(std::env::current_exe().expect("test executable"));
            command
                .args(["--exact", "storage::unified::store::impl_pages::checkpoint_work_tests::interrupted_inline_sparse_checkpoint_replays_durable_rows", "--nocapture"])
                .env(CHILD, &path)
                .env("REDDB_TEST_EXIT_AFTER_INLINE_SPARSE_WRITE", "1");
            if insert {
                command.env("REDDB_TEST_INLINE_SPARSE_INSERT", "1");
            }
            if multi {
                command.env("REDDB_TEST_INLINE_SPARSE_MULTI", "1");
            }
            let output = command.output().expect("crash child");
            assert_eq!(
                output.status.code(),
                Some(190),
                "insert={insert} multi={multi}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let recovered = UnifiedStore::open(&path).expect("recover");
            let ids: Vec<u64> = if multi {
                (1..=600).collect()
            } else {
                vec![if insert { 2 } else { 1 }]
            };
            for id in ids {
                assert_eq!(
                    recovered
                        .get("rows", EntityId::new(id))
                        .expect("recovered row")
                        .data
                        .as_row()
                        .expect("row")
                        .columns,
                    vec![Value::Integer(9)]
                );
            }
            assert_eq!(
                recovered.get_collection("rows").expect("rows").count(),
                if multi {
                    600
                } else if insert {
                    2
                } else {
                    1
                }
            );
        }
    }

    #[test]
    fn catalog_ownership_epoch_survives_reopen_and_later_publication() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        {
            let store = UnifiedStore::open(&path).expect("open store");
            store.create_collection("rows").expect("create collection");
            store.persist().expect("publish ownership epoch");
            assert_eq!(store.ownership_catalog_epoch.load(Ordering::Acquire), 1);
            store.persist().expect("publish next ownership epoch");
        }
        let reopened = UnifiedStore::open(&path).expect("reopen store");
        assert_eq!(reopened.ownership_catalog_epoch.load(Ordering::Acquire), 2);
    }

    #[test]
    fn metadata_overflow_with_missing_continuation_fails_closed() {
        use crate::storage::engine::{Page, PageType, HEADER_SIZE};

        let directory = tempfile::tempdir().expect("temporary store");
        let store = UnifiedStore::open(directory.path().join("data.rdb")).expect("paged store");
        let pager = store.pager.as_ref().unwrap();
        let valid_payload = vec![42; META_PAGE_CONTENT_CAP * 3];
        let valid_manifest = build_meta_page1_with_overflow(pager, &valid_payload).unwrap();
        pager.write_page(1, valid_manifest).unwrap();
        assert!(!existing_overflow_chain(pager).unwrap().is_empty());
        assert_eq!(read_meta_payload(pager).unwrap().unwrap(), valid_payload);

        let missing = pager.page_count().unwrap() + 100;
        let mut manifest = Page::new(PageType::Header, 1);
        reddb_file::encode_native_metadata_overflow_header(
            &mut manifest.as_bytes_mut()[HEADER_SIZE..],
            reddb_file::NativeMetadataOverflowHeader {
                format_version: STORE_VERSION_V11,
                total_payload_bytes: META_PAGE_CONTENT_CAP as u32 + 1,
                next_overflow_page_id: missing,
            },
        )
        .unwrap();
        pager.write_page(1, manifest).unwrap();

        assert!(
            existing_overflow_chain(pager).is_err(),
            "a missing continuation must not be treated as an empty chain"
        );

        let wrong_type = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let mut manifest = Page::new(PageType::Header, 1);
        reddb_file::encode_native_metadata_overflow_header(
            &mut manifest.as_bytes_mut()[HEADER_SIZE..],
            reddb_file::NativeMetadataOverflowHeader {
                format_version: STORE_VERSION_V11,
                total_payload_bytes: META_PAGE_CONTENT_CAP as u32 + 1,
                next_overflow_page_id: wrong_type.page_id(),
            },
        )
        .unwrap();
        pager.write_page(1, manifest).unwrap();
        assert!(
            existing_overflow_chain(pager).is_err(),
            "a live page must not be mistaken for metadata overflow"
        );
        assert!(
            read_meta_payload(pager).is_err(),
            "a live page must not be read as metadata overflow"
        );

        let mut cycle = pager.allocate_page(PageType::Overflow).unwrap();
        let cycle_id = cycle.page_id();
        reddb_file::encode_native_metadata_overflow_continuation_header(
            &mut cycle.as_bytes_mut()[HEADER_SIZE..],
            reddb_file::NativeMetadataOverflowContinuationHeader {
                next_overflow_page_id: cycle_id,
                chunk_bytes: 1,
            },
        )
        .unwrap();
        pager.write_page(cycle_id, cycle).unwrap();
        let mut manifest = Page::new(PageType::Header, 1);
        reddb_file::encode_native_metadata_overflow_header(
            &mut manifest.as_bytes_mut()[HEADER_SIZE..],
            reddb_file::NativeMetadataOverflowHeader {
                format_version: STORE_VERSION_V11,
                total_payload_bytes: META_V3_FIRST_PAYLOAD_CAP as u32 + 3,
                next_overflow_page_id: cycle_id,
            },
        )
        .unwrap();
        pager.write_page(1, manifest).unwrap();
        assert!(
            read_meta_payload(pager).is_err(),
            "cyclic overflow must not be accepted"
        );

        let mut manifest = Page::new(PageType::Header, 1);
        reddb_file::encode_native_metadata_overflow_header(
            &mut manifest.as_bytes_mut()[HEADER_SIZE..],
            reddb_file::NativeMetadataOverflowHeader {
                format_version: STORE_VERSION_V11,
                total_payload_bytes: META_PAGE_CONTENT_CAP as u32 + 1,
                next_overflow_page_id: 0,
            },
        )
        .unwrap();
        pager.write_page(1, manifest).unwrap();
        assert!(
            read_meta_payload(pager).is_err(),
            "a truncated metadata payload must not be accepted"
        );
    }

    #[test]
    fn retired_tree_pages_remain_pinned_by_a_cursor() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store = UnifiedStore::open(&directory.path().join("data.rdb")).expect("paged store");
        for id in 1..=100 {
            store.insert_auto("rows", row(id, 1)).expect("seed");
        }
        store.persist().expect("initial checkpoint");
        let tree = store
            .btree_indices
            .read()
            .get("rows")
            .cloned()
            .expect("tree");
        let mut cursor = tree.cursor_first().expect("cursor");
        drop(tree);
        let manager = store.get_collection("rows").expect("rows");
        for value in 2..=6 {
            manager.update(row(1, value)).expect("change");
            store.persist().expect("checkpoint with old reader");
        }
        let mut count = 0;
        while let Some((_, bytes)) = cursor.next().expect("pinned tree must stay readable") {
            let (entity, _) =
                UnifiedStore::deserialize_entity_record(&bytes, store.format_version())
                    .expect("decode");
            assert_eq!(
                entity.data.as_row().expect("row").columns,
                [Value::Integer(1)]
            );
            count += 1;
        }
        assert_eq!(count, 100);
        drop(cursor);
        store.persist().expect("reclaim after reader release");
        assert!(store.retired_btrees.lock().is_empty());
    }

    #[test]
    fn checkpoint_payload_buffer_is_bounded_for_a_large_collection() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store = UnifiedStore::open(&directory.path().join("data.rdb")).expect("paged store");
        store.create_collection("rows").expect("collection");
        let manager = store.get_collection("rows").expect("rows");
        for id in (1..=1200).rev() {
            manager
                .insert(UnifiedEntity::table_row(
                    EntityId::new(id),
                    "rows",
                    id,
                    vec![Value::text("x".repeat(4096))],
                ))
                .expect("seed");
        }
        CHECKPOINT_BUFFERED_BYTES.with(|peak| peak.set(0));
        store.persist().expect("bounded checkpoint");
        assert!(CHECKPOINT_BUFFERED_BYTES.with(|peak| peak.get()) <= CHECKPOINT_BATCH_BYTES);
        assert!(CHECKPOINT_BUFFERED_BYTES.with(|peak| peak.get()) > 0);
        assert_eq!(store.get_collection("rows").expect("rows").count(), 1200);
    }

    #[test]
    fn unchanged_checkpoint_does_not_serialize_collection_rows() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store = UnifiedStore::open(&directory.path().join("data.rdb")).expect("paged store");
        store.create_collection("rows").expect("collection");
        for id in 1..=40 {
            store
                .insert(
                    "rows",
                    UnifiedEntity::table_row(
                        EntityId::new(id),
                        "rows",
                        id,
                        vec![Value::Integer(id as i64)],
                    ),
                )
                .expect("seed row");
        }
        store.persist().expect("initial checkpoint");
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store.persist().expect("unchanged checkpoint");
        assert_eq!(
            CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()),
            0,
            "unchanged collections must not hold the writer fence while serializing every row"
        );
        drop(store);
        let reopened = UnifiedStore::open(&directory.path().join("data.rdb")).expect("reopen");
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        reopened.persist().expect("unchanged recovered generation");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 0);
    }

    #[test]
    fn sparse_wal_replay_does_not_reencode_complete_collections() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        for (name, first) in [("rows", 1), ("cold", 129)] {
            store.create_collection(name).expect("collection");
            for id in first..first + 128 {
                store
                    .insert(
                        name,
                        UnifiedEntity::table_row(
                            EntityId::new(id),
                            name,
                            id,
                            vec![Value::text(format!("{id}:{}", "x".repeat(8192)))],
                        ),
                    )
                    .expect("seed");
            }
        }
        store.persist().expect("durable baseline");
        let action = super::super::commit::StoreWalAction::upsert_entity(
            "rows",
            &row(1, 42),
            None,
            STORE_VERSION_V11,
        );
        // The acknowledged WAL image has not reached its home B-tree yet.
        store
            .commit
            .as_ref()
            .unwrap()
            .append_actions(&[action])
            .unwrap();
        drop(store);

        REPLAY_SERIALIZED_ROWS.with(|count| count.set(0));
        let recovered = UnifiedStore::open(&path).expect("replay committed change");
        assert_eq!(
            recovered
                .get("rows", EntityId::new(1))
                .unwrap()
                .data
                .as_row()
                .unwrap()
                .columns,
            vec![Value::Integer(42)]
        );
        for name in ["rows", "cold"] {
            assert_eq!(recovered.get_collection(name).unwrap().count(), 128);
        }
        assert_eq!(
            REPLAY_SERIALIZED_ROWS.with(|count| count.get()),
            0,
            "one WAL-backed row must not reencode its whole collection or unrelated cold rows"
        );
        for (name, first) in [("rows", 2), ("cold", 129)] {
            let end = if name == "rows" { 129 } else { 257 };
            for id in first..end {
                assert_eq!(
                    recovered
                        .get(name, EntityId::new(id))
                        .unwrap()
                        .data
                        .as_row()
                        .unwrap()
                        .columns,
                    vec![Value::text(format!("{id}:{}", "x".repeat(8192)))],
                );
            }
        }

        drop(recovered);
        let reopened = UnifiedStore::open(&path).expect("durable sparse replay");
        assert_eq!(
            reopened
                .get("rows", EntityId::new(1))
                .unwrap()
                .data
                .as_row()
                .unwrap()
                .columns,
            vec![Value::Integer(42)]
        );
    }

    fn replay_test_payload(bytes: usize) -> String {
        let mut state = 0x1234_5678_u32;
        (0..bytes)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                char::from(b'!' + (state % 90) as u8)
            })
            .collect()
    }

    #[test]
    fn wal_replay_with_exhausted_row_proof_keeps_full_validation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.rdb");
        let config = || UnifiedStoreConfig::default().with_page_cache_slots(4);
        let store = UnifiedStore::open_with_config(&path, config()).unwrap();
        for id in 1..=16 {
            store.insert_auto("rows", row(id, 1)).unwrap();
        }
        store.persist().unwrap();
        // Larger than the retained-image allowance, so replay must fall back.
        let payload = replay_test_payload(64 * 1024);
        let changed =
            UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![Value::text(payload)]);
        let action = StoreWalAction::upsert_entity("rows", &changed, None, store.format_version());
        store
            .commit
            .as_ref()
            .unwrap()
            .append_actions(&[action])
            .unwrap();
        drop(store);
        REPLAY_SERIALIZED_ROWS.with(|count| count.set(0));
        let recovered = UnifiedStore::open_with_config(&path, config()).unwrap();
        assert!(REPLAY_SERIALIZED_ROWS.with(|count| count.get()) >= 16);
        assert_eq!(
            recovered
                .get("rows", EntityId::new(1))
                .unwrap()
                .data
                .as_row()
                .unwrap()
                .columns,
            changed.data.as_row().unwrap().columns
        );
        drop(recovered);
        let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
        assert_eq!(reopened.get_collection("rows").unwrap().count(), 16);
        assert_eq!(
            reopened
                .get("rows", EntityId::new(1))
                .unwrap()
                .data
                .as_row()
                .unwrap()
                .columns,
            changed.data.as_row().unwrap().columns
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interrupted_sparse_replay_preserves_rows_and_bounds_growth() {
        const CHILD: &str = "REDDB_TEST_SPARSE_REPLAY_PATH";
        if let Some(path) = std::env::var_os(CHILD) {
            let _store = UnifiedStore::open(path).unwrap();
            panic!("replay crash hook was not reached");
        }
        for overflow in [false, true] {
            for (hook, code) in [
                ("REDDB_TEST_EXIT_AFTER_SPARSE_BATCH", 193),
                ("REDDB_EXPERIMENTAL_EXIT_AFTER_REPLAY_PUBLICATION", 182),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("data.rdb");
                let store = UnifiedStore::open(&path).unwrap();
                for id in 1..=128 {
                    store.insert_auto("rows", row(id, 1)).unwrap();
                }
                store.persist().unwrap();
                let changed = if overflow {
                    UnifiedEntity::table_row(
                        EntityId::new(1),
                        "rows",
                        1,
                        vec![Value::text(replay_test_payload(16 * 1024))],
                    )
                } else {
                    row(1, 42)
                };
                let action =
                    StoreWalAction::upsert_entity("rows", &changed, None, store.format_version());
                store
                    .commit
                    .as_ref()
                    .unwrap()
                    .append_actions(&[action])
                    .unwrap();
                drop(store);
                let mut first_size = 0;
                for cycle in 0..12 {
                    let output = std::process::Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", "storage::unified::store::impl_pages::checkpoint_work_tests::interrupted_sparse_replay_preserves_rows_and_bounds_growth", "--nocapture"])
                        .env(CHILD, &path).env(hook, "1").output().unwrap();
                    assert_eq!(
                        output.status.code(),
                        Some(code),
                        "overflow={overflow} hook={hook} cycle={cycle}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let size = std::fs::metadata(&path).unwrap().len();
                    if cycle == 0 {
                        first_size = size;
                    }
                    assert!(
                        size <= first_size + 2 * crate::storage::engine::PAGE_SIZE as u64,
                        "repeated recovery must reuse reservations: {first_size} -> {size}"
                    );
                }
                for _ in 0..2 {
                    let recovered = UnifiedStore::open(&path).unwrap();
                    assert_eq!(recovered.get_collection("rows").unwrap().count(), 128);
                    for id in 1..=128 {
                        let expected = if id == 1 { changed.clone() } else { row(id, 1) };
                        assert_eq!(
                            recovered
                                .get("rows", EntityId::new(id))
                                .unwrap()
                                .data
                                .as_row()
                                .unwrap()
                                .columns,
                            expected.data.as_row().unwrap().columns
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn paged_reopen_preserves_stored_sequence_identity() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        store.create_collection("rows").expect("collection");
        for id in [30, 10, 20] {
            store.insert("rows", row(id, id as i64)).expect("insert");
        }
        store.persist().expect("checkpoint");
        let sequences = [10, 20, 30].map(|id| {
            store
                .get("rows", EntityId::new(id))
                .expect("row")
                .sequence_id
        });
        drop(store);
        let reopened = UnifiedStore::open(&path).expect("reopen");
        let recovered = [10, 20, 30].map(|id| {
            reopened
                .get("rows", EntityId::new(id))
                .expect("row")
                .sequence_id
        });
        assert_eq!(
            recovered, sequences,
            "opening a checkpoint must not rewrite stored row identities"
        );
    }

    #[test]
    fn auto_insert_persists_the_assigned_sequence_identity() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store = UnifiedStore::open(&directory.path().join("data.rdb")).expect("paged store");
        for id in 1u64..=3 {
            store
                .insert_auto("rows", row(id, id as i64))
                .expect("insert");
        }
        let manager = store.get_collection("rows").expect("rows");
        let tree = store
            .btree_indices
            .read()
            .get("rows")
            .cloned()
            .expect("tree");
        for id in 1u64..=3 {
            let bytes = tree
                .get(&id.to_be_bytes())
                .expect("lookup")
                .expect("stored row");
            let (persisted, _) =
                UnifiedStore::deserialize_entity_record(&bytes, store.format_version())
                    .expect("decode");
            assert_eq!(
                persisted.sequence_id,
                manager
                    .get(EntityId::new(id))
                    .expect("live row")
                    .sequence_id
            );
        }
    }

    #[test]
    fn completed_inline_insert_reuses_history_but_not_untracked_edits() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        store.insert_auto("rows", row(1, 1)).expect("seed");
        store.persist().expect("checkpoint");
        store.insert_auto("rows", row(2, 2)).expect("append");
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store.persist().expect("inline append checkpoint");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 0);
        store
            .get_collection("rows")
            .expect("rows")
            .update(row(1, 99))
            .expect("untracked edit");
        store.insert_auto("rows", row(3, 3)).expect("next append");
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store.persist().expect("untracked edit checkpoint");
        assert!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()) > 0);
        drop(store);
        let reopened = UnifiedStore::open(&path).expect("reopen");
        assert_eq!(
            reopened
                .get("rows", EntityId::new(1))
                .expect("row")
                .data
                .as_row()
                .expect("table")
                .columns,
            vec![Value::Integer(99)]
        );
    }

    #[test]
    fn replayed_trees_are_reusable_after_durable_reconciliation() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        store.insert_auto("rows", row(1, 1)).expect("seed");
        store.persist().expect("checkpoint");
        store
            .insert_auto("rows", row(2, 2))
            .expect("WAL-backed append");
        drop(store);
        let reopened = UnifiedStore::open(&path).expect("replay");
        assert!(reopened.get("rows", EntityId::new(2)).is_some());
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        reopened.persist().expect("checkpoint reconciled tree");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 0);
    }

    fn row(id: u64, value: i64) -> UnifiedEntity {
        UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![Value::Integer(value)])
    }

    #[test]
    fn checkpoint_reuse_tracks_manager_updates_metadata_deletes_and_bulk_inserts() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        store.create_collection("rows").expect("collection");
        store.insert("rows", row(1, 1)).expect("seed");
        store.persist().expect("initial checkpoint");
        let manager = store.get_collection("rows").expect("rows");
        manager.update(row(1, 2)).expect("direct update");
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store.persist().expect("updated row checkpoint");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 1);
        let mut metadata = Metadata::new();
        metadata.set("verified", MetadataValue::Bool(true));
        manager
            .set_metadata(EntityId::new(1), metadata)
            .expect("metadata");
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store.persist().expect("metadata-only checkpoint");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 1);
        manager
            .bulk_insert(vec![row(2, 2), row(3, 3)])
            .expect("bulk insert");
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store.persist().expect("bulk insert checkpoint");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 3);
        manager.delete(EntityId::new(2)).expect("delete");
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store.persist().expect("changed checkpoint");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 2);
        drop(store);
        let recovered = UnifiedStore::open(&path).expect("recover changed rows");
        let manager = recovered.get_collection("rows").expect("rows");
        assert_eq!(manager.count(), 2);
        assert_eq!(
            manager
                .get(EntityId::new(1))
                .expect("row")
                .data
                .as_row()
                .expect("table")
                .columns,
            vec![Value::Integer(2)]
        );
        assert!(manager.get(EntityId::new(2)).is_none());
        assert!(manager
            .get_metadata(EntityId::new(1))
            .expect("metadata")
            .get("verified")
            .is_some());
    }

    #[test]
    fn unrelated_visibility_changes_do_not_serialize_collection_rows() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store = UnifiedStore::open(&directory.path().join("data.rdb")).expect("paged store");
        let snapshots = Arc::new(crate::storage::transaction::snapshot::SnapshotManager::new());
        assert!(store.snapshot_manager.set(Arc::clone(&snapshots)).is_ok());
        store.create_collection("rows").expect("collection");
        for id in 1..=40 {
            store.insert("rows", row(id, id as i64)).expect("seed");
        }
        store.persist().expect("initial checkpoint");
        let unrelated = snapshots.begin();
        snapshots.rollback(unrelated);
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store.persist().expect("unrelated rollback checkpoint");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 0);

        store
            .pending_vault_versions
            .write()
            .insert(EntityId::new(100));
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store.persist().expect("unrelated vault checkpoint");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 0);
        store.pending_vault_versions.write().clear();
        CHECKPOINT_SERIALIZED_ROWS.with(|count| count.set(0));
        store
            .persist()
            .expect("unrelated vault publication checkpoint");
        assert_eq!(CHECKPOINT_SERIALIZED_ROWS.with(|count| count.get()), 0);
    }

    #[test]
    fn low_level_rollback_of_a_non_active_xid_invalidates_sparse_visibility() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        let snapshots = Arc::new(crate::storage::transaction::snapshot::SnapshotManager::new());
        assert!(store.snapshot_manager.set(Arc::clone(&snapshots)).is_ok());
        store.create_collection("rows").expect("collection");
        let xid = snapshots.begin();
        let mut entity = row(1, 1);
        entity.xmin = xid;
        store.insert("rows", entity).expect("committed row");
        snapshots.commit(xid);
        store.persist().expect("committed baseline");
        snapshots.rollback(xid);
        store
            .persist()
            .expect("untracked outcome requires validation");
        drop(store);
        let recovered = UnifiedStore::open(&path).expect("recover");
        assert_eq!(recovered.get_collection("rows").expect("rows").count(), 0);
    }

    #[test]
    fn exhausted_deferred_row_cache_keeps_full_validation() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        let snapshots = Arc::new(crate::storage::transaction::snapshot::SnapshotManager::new());
        assert!(store.snapshot_manager.set(Arc::clone(&snapshots)).is_ok());
        store.create_collection("rows").expect("collection");
        let xid = snapshots.begin();
        for id in 1..=1025 {
            let mut entity = row(id, id as i64);
            entity.xmin = xid;
            store.insert("rows", entity).expect("pending row");
        }
        store.persist().expect("exclude pending rows");
        assert!(store.checkpoint_versions.read()["rows"]
            .deferred_rows
            .is_none());
        snapshots.commit(xid);
        store
            .persist()
            .expect("publish committed rows conservatively");
        drop(store);
        let recovered = UnifiedStore::open(&path).expect("recover");
        assert_eq!(
            recovered.get_collection("rows").expect("rows").count(),
            1025
        );
    }

    #[test]
    fn rollback_invalidates_reuse_without_a_segment_mutation() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        let snapshots = Arc::new(crate::storage::transaction::snapshot::SnapshotManager::new());
        assert!(store.snapshot_manager.set(Arc::clone(&snapshots)).is_ok());
        store.create_collection("rows").expect("collection");
        let xid = snapshots.begin();
        let mut pending = row(1, 1);
        pending.xmin = xid;
        store.insert("rows", pending).expect("version");
        store.persist().expect("before rollback");
        snapshots.rollback(xid);
        store.persist().expect("after rollback");
        drop(store);
        let recovered = UnifiedStore::open(&path).expect("recover");
        assert_eq!(recovered.get_collection("rows").expect("rows").count(), 0);
    }

    #[test]
    fn rolled_back_delete_is_normalized_without_a_segment_mutation() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        let snapshots = Arc::new(crate::storage::transaction::snapshot::SnapshotManager::new());
        assert!(store.snapshot_manager.set(Arc::clone(&snapshots)).is_ok());
        store.create_collection("rows").expect("collection");
        let xid = snapshots.begin();
        let mut deleted = row(1, 1);
        deleted.xmax = xid;
        store.insert("rows", deleted).expect("version");
        store.persist().expect("before rollback");
        snapshots.rollback(xid);
        store.persist().expect("after rollback");
        drop(store);
        let recovered = UnifiedStore::open(&path).expect("recover");
        let manager = recovered.get_collection("rows").expect("rows");
        assert_eq!(manager.get(EntityId::new(1)).expect("retained row").xmax, 0);
    }

    #[test]
    fn publishing_a_pending_vault_version_invalidates_reuse() {
        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let store = UnifiedStore::open(&path).expect("paged store");
        store.create_collection("rows").expect("collection");
        store.insert("rows", row(1, 1)).expect("row");
        store
            .pending_vault_versions
            .write()
            .insert(EntityId::new(1));
        store.persist().expect("exclude pending vault version");
        store
            .pending_vault_versions
            .write()
            .remove(&EntityId::new(1));
        store.persist().expect("include published vault version");
        drop(store);
        let recovered = UnifiedStore::open(&path).expect("recover");
        assert_eq!(recovered.get_collection("rows").expect("rows").count(), 1);
    }
}
