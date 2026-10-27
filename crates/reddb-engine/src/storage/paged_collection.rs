//! Fallible reads of physical entity records without a resident collection.
//!
//! This owns tree handles, never a decoded entity cache. Each read materializes
//! one existing B-tree value; the caller owns its admission and lifetime. The
//! record can still be large; this is not a read-workspace or process RAM bound.
//! A tree/cursor retirement pin prevents reclamation, not in-place mutation.
//! Callers must fence writers or supply an immutable published generation.
use super::{
    btree::{BTree, BTreeCursor, BTreeError},
    entity::{EntityId, UnifiedEntity},
    error::{StorageIntegrityError, StoreError},
    metadata::Metadata,
    metadata_codec::deserialize_entity_record,
    page_store::PageStore,
};
use std::sync::Arc;

pub struct PagedEntityRecord {
    pub entity: UnifiedEntity,
    pub metadata: Option<Metadata>,
}

/// Physical versions are returned unchanged. MVCC visibility belongs to the caller.
pub struct PagedCollectionReader<P: PageStore> {
    tree: Arc<BTree<P>>,
    collection: Arc<str>,
    format_version: u32,
}

impl<P: PageStore> PagedCollectionReader<P> {
    /// Attach a known tree without reading or decoding any of its records.
    pub fn new(
        tree: Arc<BTree<P>>,
        collection: impl Into<Arc<str>>,
        format_version: u32,
    ) -> Result<Self, StoreError> {
        if !reddb_file::is_supported_store_version(format_version) {
            return Err(StoreError::Serialization(format!(
                "unsupported paged entity format: {format_version}"
            )));
        }
        Ok(Self {
            tree,
            collection: collection.into(),
            format_version,
        })
    }

    pub fn get(&self, id: EntityId) -> Result<Option<PagedEntityRecord>, StoreError> {
        let key = id.raw().to_be_bytes();
        let value = self
            .tree
            .get(&key)
            .map_err(|error| self.tree_error(error))?;
        value.map(|value| self.decode(&key, &value)).transpose()
    }

    pub fn cursor(&self) -> Result<PagedCollectionCursor<P>, StoreError> {
        let cursor = self
            .tree
            .cursor_first()
            .map_err(|error| self.tree_error(error))?;
        Ok(PagedCollectionCursor {
            reader: Self {
                tree: Arc::clone(&self.tree),
                collection: Arc::clone(&self.collection),
                format_version: self.format_version,
            },
            cursor,
            failed: false,
        })
    }

    fn tree_error(&self, error: BTreeError) -> StoreError {
        // The current B-tree erases the pager's concrete error type, including
        // checksum failures. Retain native open's integrity classification and
        // the original cause instead of guessing I/O versus damaged bytes.
        self.integrity_error(error.to_string())
    }

    fn integrity_error(&self, detail: impl Into<String>) -> StoreError {
        StoreError::StorageIntegrity(StorageIntegrityError::new(
            "page",
            self.tree.root_page_id().to_string(),
            Some(self.collection.to_string()),
            detail,
        ))
    }

    fn decode(&self, key: &[u8], value: &[u8]) -> Result<PagedEntityRecord, StoreError> {
        let key: [u8; 8] = key
            .try_into()
            .map_err(|_| self.integrity_error("physical entity key is not eight bytes"))?;
        let (entity, metadata) = deserialize_entity_record(value, self.format_version)
            .map_err(|error| self.integrity_error(error))?;
        if entity.id.raw() != u64::from_be_bytes(key) {
            return Err(self.integrity_error(format!(
                "physical key {} does not match entity {}",
                u64::from_be_bytes(key),
                entity.id.raw()
            )));
        }
        Ok(PagedEntityRecord { entity, metadata })
    }
}

/// Pins the source tree and yields one owned record at a time in physical-ID order.
pub struct PagedCollectionCursor<P: PageStore> {
    reader: PagedCollectionReader<P>,
    cursor: BTreeCursor<P>,
    failed: bool,
}

impl<P: PageStore> PagedCollectionCursor<P> {
    /// Errors fence the cursor; a damaged record is never skipped as absent/EOF.
    pub fn next(&mut self) -> Result<Option<PagedEntityRecord>, StoreError> {
        if self.failed {
            return Err(StoreError::Internal(
                "paged entity cursor is fenced after a read failure".into(),
            ));
        }
        let result = self
            .cursor
            .next()
            .map_err(|error| self.reader.tree_error(error))
            .and_then(|record| {
                record
                    .map(|(key, value)| self.reader.decode(&key, &value))
                    .transpose()
            });
        self.failed = result.is_err();
        result
    }
}
