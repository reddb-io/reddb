//! Lazy physical postings over a pinned immutable secondary-index generation.
//! A cursor retains one leaf and a bounded key, never the complete posting list.
use super::btree::{BTree, BTreeCursor};
use super::collection_read::read_error;
use super::entity::EntityId;
use super::error::{StorageIntegrityError, StoreError};
use super::page_store::PageStore;
use std::sync::Arc;

pub const SECONDARY_KEY_BYTES_MAX: usize = 1016;

/// The publishing host must never mutate the tree while this handle is live.
/// Exposing only read cursors keeps query callers away from mutable tree APIs.
pub struct SecondaryIndexRead<P: PageStore> {
    tree: Arc<BTree<P>>,
    source: Option<Arc<BTree<P>>>,
}

impl<P: PageStore> Clone for SecondaryIndexRead<P> {
    fn clone(&self) -> Self {
        Self {
            tree: Arc::clone(&self.tree),
            source: self.source.clone(),
        }
    }
}

impl<P: PageStore> SecondaryIndexRead<P> {
    pub fn new(tree: Arc<BTree<P>>) -> Self {
        Self { tree, source: None }
    }

    /// Pin the paired data tree too, so a posting cursor cannot outlive the
    /// physical records its generation references during concurrent retirement.
    pub fn with_source(tree: Arc<BTree<P>>, source: Arc<BTree<P>>) -> Self {
        Self {
            tree,
            source: Some(source),
        }
    }

    pub fn root_page_id(&self) -> u32 {
        self.tree.root_page_id()
    }

    /// Visit compact projected keys without reading source records or posting
    /// values. The immutable handle pins both trees for the entire traversal.
    /// Retains one leaf and at most one bounded key, independent of row count.
    pub fn try_for_each(
        &self,
        mut visit: impl FnMut(&[u8], EntityId) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut cursor = self
            .tree
            .cursor_seek(&[])
            .map_err(read_error)?
            .with_immutable_leaves();
        while let Some(result) = cursor
            .peek_key_with(|posting| {
                if !(8..=SECONDARY_KEY_BYTES_MAX + 8).contains(&posting.len()) {
                    return Err(StoreError::StorageIntegrity(StorageIntegrityError::new(
                        "secondary-index",
                        "posting",
                        None,
                        "invalid projected key length",
                    )));
                }
                let split = posting.len() - 8;
                let id = EntityId::new(u64::from_be_bytes(
                    posting[split..]
                        .try_into()
                        .expect("invariant: eight-byte identity"),
                ));
                visit(&posting[..split], id)
            })
            .map_err(read_error)?
        {
            result?;
            cursor.skip_next().map_err(read_error)?;
        }
        Ok(())
    }

    /// Return physical candidates for exactly these projected bytes. A hashed
    /// projection still requires full-key verification by its caller. Both
    /// MVCC visibility and the dirty overlay are outside this base-only reader.
    pub fn candidates(&self, key: &[u8]) -> Result<SecondaryIndexCursor<P>, StoreError> {
        if key.len() > SECONDARY_KEY_BYTES_MAX {
            return Err(StoreError::ReadLimitExceeded {
                required_bytes: key.len(),
                limit_bytes: SECONDARY_KEY_BYTES_MAX,
            });
        }
        let mut start = [0; SECONDARY_KEY_BYTES_MAX + 8];
        start[..key.len()].copy_from_slice(key);
        let cursor = self
            .tree
            .cursor_seek(&start[..key.len() + 8])
            .map_err(read_error)?
            .with_immutable_leaves();
        Ok(SecondaryIndexCursor {
            cursor,
            key: key.to_vec(),
            finished: false,
            _source: self.source.clone(),
        })
    }
}

/// Each step is fallible: unreadable pages must never mean an empty index.
pub struct SecondaryIndexCursor<P: PageStore> {
    cursor: BTreeCursor<P>,
    key: Vec<u8>,
    finished: bool,
    _source: Option<Arc<BTree<P>>>,
}

impl<P: PageStore> SecondaryIndexCursor<P> {
    pub fn next_id(&mut self) -> Result<Option<EntityId>, StoreError> {
        if self.finished {
            return Ok(None);
        }
        while let Some(posting) = self.cursor.peek_key().map_err(read_error)? {
            if posting.len() < 8 {
                return Err(StoreError::StorageIntegrity(StorageIntegrityError::new(
                    "secondary-index",
                    "posting",
                    None,
                    "posting has no physical identity",
                )));
            }
            if !posting.starts_with(&self.key) {
                self.finished = true;
                return Ok(None);
            }
            // Prefix-adjacent variable-width keys may interleave with physical
            // IDs. They are not equal to this projected key and must be skipped.
            let id = (posting.len() == self.key.len() + 8).then(|| {
                EntityId::new(u64::from_be_bytes(
                    posting[self.key.len()..]
                        .try_into()
                        .expect("invariant: eight-byte identity"),
                ))
            });
            self.cursor.skip_next().map_err(read_error)?;
            if id.is_some() {
                return Ok(id);
            }
        }
        self.finished = true;
        Ok(None)
    }
}
