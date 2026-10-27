//! Build an unpublished tree from a complete physical read authority.
//!
//! The host fences the source and owns WAL/DWB/manifest publication and tree
//! retirement. Success here does not publish roots or make pages durable.
use super::{
    btree::{BTree, BTreeError},
    collection_read::CollectionReadView,
    entity::UnifiedEntity,
    error::StoreError,
    metadata::Metadata,
    page_store::PageStore,
};
use std::sync::Arc;

pub const CHECKPOINT_BATCH_BYTES: usize = 1024 * 1024;
pub const CHECKPOINT_BATCH_ROWS: usize = 512;

pub struct CheckpointTreeBuild<P: PageStore> {
    pub tree: Arc<BTree<P>>,
    /// Encoded batch ownership only; a single larger value retains its existing
    /// size contract. This is not a pager, decoder or process-memory ceiling.
    pub peak_buffered_bytes: usize,
}

struct Builder<P: PageStore> {
    tree: Arc<BTree<P>>,
    records: Vec<(Vec<u8>, Vec<u8>)>,
    bytes: usize,
    peak: usize,
}

impl<P: PageStore> Builder<P> {
    fn new(pages: &Arc<P>) -> Self {
        Self {
            tree: Arc::new(BTree::new(Arc::clone(pages))),
            records: Vec::new(),
            bytes: 0,
            peak: 0,
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
        self.peak = self.peak.max(self.bytes);
        self.records.push(record);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StoreError> {
        self.tree
            .bulk_insert_sorted(&self.records)
            .map_err(tree_error)?;
        self.records.clear();
        self.bytes = 0;
        Ok(())
    }

    fn copy_prefix(&mut self, previous: &BTree<P>, count: usize) -> Result<(), StoreError> {
        let mut cursor = previous.cursor_first().map_err(tree_error)?;
        for _ in 0..count {
            let record = cursor
                .next()
                .map_err(tree_error)?
                .ok_or_else(|| StoreError::Serialization("checkpoint prefix disappeared".into()))?;
            self.push(record)?;
        }
        Ok(())
    }
}

fn tree_error(error: BTreeError) -> StoreError {
    StoreError::Io(std::io::Error::other(error.to_string()))
}

/// Compare the complete source in physical-ID order. Reuse an identical tree;
/// on its first difference copy the verified prefix into a bounded replacement.
/// Encode may omit/normalize rows using the host's frozen transaction state.
/// Source failures return Err, including after a prefix; no root is published.
pub fn build_checkpoint_tree<P: PageStore>(
    pages: &Arc<P>,
    source: &CollectionReadView<P>,
    previous: Option<&Arc<BTree<P>>>,
    mut encode: impl FnMut(&UnifiedEntity, Option<&Metadata>) -> Option<Vec<u8>>,
) -> Result<CheckpointTreeBuild<P>, StoreError> {
    let mut cursor = previous
        .map(|tree| tree.cursor_first())
        .transpose()
        .map_err(tree_error)?;
    let mut matched = 0;
    let mut builder = None;
    let mut failure = None;
    source.visit_records(|entity, metadata| {
        let Some(value) = encode(entity, metadata) else {
            return true;
        };
        let record = (entity.id.raw().to_be_bytes().to_vec(), value);
        let result = (|| {
            if builder.is_none() {
                if let Some(cursor) = cursor.as_mut() {
                    if cursor.next().map_err(tree_error)?.as_ref() == Some(&record) {
                        matched += 1;
                        return Ok(());
                    }
                }
                let mut replacement = Builder::new(pages);
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
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    if builder.is_none() {
        if let (Some(previous), Some(cursor)) = (previous, cursor.as_mut()) {
            if cursor.next().map_err(tree_error)?.is_none() {
                return Ok(CheckpointTreeBuild {
                    tree: Arc::clone(previous),
                    peak_buffered_bytes: 0,
                });
            }
        }
        let mut replacement = Builder::new(pages);
        if let Some(previous) = previous {
            replacement.copy_prefix(previous, matched)?;
        }
        builder = Some(replacement);
    }
    let mut builder = builder.expect("replacement or reused tree");
    builder.flush()?;
    Ok(CheckpointTreeBuild {
        tree: builder.tree,
        peak_buffered_bytes: builder.peak,
    })
}
