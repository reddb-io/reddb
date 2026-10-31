//! Candidate-only seeks over an immutable collection and its paired key index.
use super::collection_read::{read_error, CollectionCursor, EncodedRecord};
use crate::storage::btree::{BTree, BTreeCursor};
use crate::storage::entity::EntityId;
use crate::storage::error::{StorageIntegrityError, StoreError};
use crate::storage::page_store::PageStore;
use reddb_file::native_key_index;
use std::sync::Arc;

pub(super) struct KeyCursor<P: PageStore> {
    records: Arc<BTree<P>>,
    candidates: BTreeCursor<P>,
    fingerprint: [u8; native_key_index::FINGERPRINT_BYTES],
}

impl<P: PageStore> KeyCursor<P> {
    pub(super) fn new(
        records: Arc<BTree<P>>,
        index: &BTree<P>,
        key: &str,
    ) -> Result<Self, StoreError> {
        let fingerprint = native_key_index::fingerprint(key);
        let start = native_key_index::encode_candidate(&fingerprint, 0);
        let candidates = index.cursor_seek(&start).map_err(read_error)?;
        Ok(Self {
            records,
            candidates,
            fingerprint,
        })
    }
}

impl<P: PageStore> CollectionCursor for KeyCursor<P> {
    fn peek_id(&mut self) -> Result<Option<EntityId>, StoreError> {
        let Some(key) = self.candidates.peek_key().map_err(read_error)? else {
            return Ok(None);
        };
        let (fingerprint, id) = native_key_index::decode_candidate(&key).map_err(|error| {
            StoreError::StorageIntegrity(StorageIntegrityError::new(
                "collection-key-index",
                "candidate",
                None,
                error.to_string(),
            ))
        })?;
        Ok((fingerprint == self.fingerprint).then(|| EntityId::new(id)))
    }

    fn next_record(&mut self, limit_bytes: usize) -> Result<Option<EncodedRecord>, StoreError> {
        let Some(id) = self.peek_id()? else {
            return Ok(None);
        };
        let bytes = self
            .records
            .get_with_budget(&id.raw().to_be_bytes(), limit_bytes)
            .map_err(read_error)?
            .ok_or_else(|| {
                StoreError::StorageIntegrity(StorageIntegrityError::new(
                    "collection-key-index",
                    id.raw().to_string(),
                    None,
                    "candidate references a missing physical record",
                ))
            })?;
        // Read/limit failures above preserve the candidate for a bounded retry.
        self.candidates.skip_next().map_err(read_error)?;
        Ok(Some(EncodedRecord { id, bytes }))
    }

    fn skip_record(&mut self) -> Result<bool, StoreError> {
        if self.peek_id()?.is_none() {
            return Ok(false);
        }
        self.candidates.skip_next().map_err(read_error)
    }
}
