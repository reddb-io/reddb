//! Fallible, ordered reads over a published collection and a frozen write overlay.
//!
//! Hosts own publication: a base tree must remain immutable while a view uses it.
//! No reader hydrates the complete collection or treats failed I/O as a missing row.

use super::btree::{BTree, BTreeCursor, BTreeError};
use super::entity::{EntityId, UnifiedEntity};
use super::error::{StorageIntegrityError, StoreError};
use super::metadata::Metadata;
use super::metadata_codec::deserialize_entity_record;
use super::page_store::PageStore;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct EncodedRecord {
    pub id: EntityId,
    pub bytes: Vec<u8>,
}

impl EncodedRecord {
    /// The allowance covers encoded input plus retained decoded allocations.
    /// A host must reserve the same allowance in its global scratch pool.
    pub fn decode_with_budget(
        &self,
        version: u32,
        limit_bytes: usize,
    ) -> Result<(UnifiedEntity, Option<Metadata>), StoreError> {
        let mut budget = reddb_file::native_value::DecodeBudget::new(limit_bytes);
        budget
            .charge(self.bytes.len())
            .map_err(super::metadata_codec::decode_error)?;
        let record = super::metadata_codec::deserialize_entity_record_with_budget(
            &self.bytes,
            version,
            &mut budget,
        )?;
        self.validate_identity(record)
    }

    /// Typed decoding is separate from byte admission; hosts must also reserve
    /// space for decoded values in their shared query memory account.
    pub fn decode(&self, version: u32) -> Result<(UnifiedEntity, Option<Metadata>), StoreError> {
        let record =
            deserialize_entity_record(&self.bytes, version).map_err(StoreError::Serialization)?;
        self.validate_identity(record)
    }

    fn validate_identity(
        &self,
        record: (UnifiedEntity, Option<Metadata>),
    ) -> Result<(UnifiedEntity, Option<Metadata>), StoreError> {
        if record.0.id != self.id {
            return Err(StoreError::StorageIntegrity(StorageIntegrityError::new(
                "collection-record",
                self.id.raw().to_string(),
                None,
                "entity identity differs from its physical key",
            )));
        }
        Ok(record)
    }
}

pub trait CollectionCursor {
    fn peek_id(&mut self) -> Result<Option<EntityId>, StoreError>;
    fn next_record(&mut self, limit_bytes: usize) -> Result<Option<EncodedRecord>, StoreError>;
    fn skip_record(&mut self) -> Result<bool, StoreError>;
}

pub trait CollectionRead {
    fn get_record(
        &self,
        id: EntityId,
        limit_bytes: usize,
    ) -> Result<Option<EncodedRecord>, StoreError>;
    fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError>;
    /// The cursor retains its page-retirement fence independently of this handle.
    fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError>;
    /// Candidate index for this exact immutable base, or None for an unindexed
    /// legacy/fixture source. Callers still verify each decoded logical key.
    fn key_cursor(&self, _key: &str) -> Result<Option<Box<dyn CollectionCursor + '_>>, StoreError> {
        Ok(None)
    }
    fn owned_key_cursor(
        &self,
        _key: &str,
    ) -> Result<Option<Box<dyn CollectionCursor>>, StoreError> {
        Ok(None)
    }
}

/// A published generation; the host retains its existing page-retirement fence.
pub struct PagedCollection<P: PageStore> {
    tree: Arc<BTree<P>>,
    keys: Option<Arc<BTree<P>>>,
}

impl<P: PageStore> PagedCollection<P> {
    pub fn new(tree: Arc<BTree<P>>) -> Self {
        Self { tree, keys: None }
    }

    /// Hosts publish both roots atomically and retain their retirement fences.
    pub fn with_key_index(tree: Arc<BTree<P>>, keys: Arc<BTree<P>>) -> Self {
        Self {
            tree,
            keys: Some(keys),
        }
    }

    pub fn scan(&self) -> Result<PagedCursor<P>, StoreError> {
        Ok(PagedCursor {
            cursor: self.tree.cursor_first().map_err(read_error)?,
        })
    }
}

impl<P: PageStore + 'static> CollectionRead for PagedCollection<P> {
    fn get_record(
        &self,
        id: EntityId,
        limit_bytes: usize,
    ) -> Result<Option<EncodedRecord>, StoreError> {
        self.tree
            .get_with_budget(&id.raw().to_be_bytes(), limit_bytes)
            .map(|record| record.map(|bytes| EncodedRecord { id, bytes }))
            .map_err(read_error)
    }

    fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
        Ok(Box::new(self.scan()?))
    }

    fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
        Ok(Box::new(self.scan()?))
    }

    fn key_cursor(&self, key: &str) -> Result<Option<Box<dyn CollectionCursor + '_>>, StoreError> {
        let Some(keys) = &self.keys else {
            return Ok(None);
        };
        Ok(Some(Box::new(super::collection_key_read::KeyCursor::new(
            Arc::clone(&self.tree),
            keys,
            key,
        )?)))
    }

    fn owned_key_cursor(&self, key: &str) -> Result<Option<Box<dyn CollectionCursor>>, StoreError> {
        let Some(keys) = &self.keys else {
            return Ok(None);
        };
        Ok(Some(Box::new(super::collection_key_read::KeyCursor::new(
            Arc::clone(&self.tree),
            keys,
            key,
        )?)))
    }
}

pub struct PagedCursor<P: PageStore> {
    cursor: BTreeCursor<P>,
}

impl<P: PageStore> CollectionCursor for PagedCursor<P> {
    fn peek_id(&mut self) -> Result<Option<EntityId>, StoreError> {
        self.cursor
            .peek_key()
            .map_err(read_error)?
            .map(|key| physical_id(&key))
            .transpose()
    }

    fn next_record(&mut self, limit_bytes: usize) -> Result<Option<EncodedRecord>, StoreError> {
        self.cursor
            .next_with_budget(limit_bytes)
            .map_err(read_error)?
            .map(|(key, bytes)| {
                Ok(EncodedRecord {
                    id: physical_id(&key)?,
                    bytes,
                })
            })
            .transpose()
    }

    fn skip_record(&mut self) -> Result<bool, StoreError> {
        self.cursor.skip_next().map_err(read_error)
    }
}

/// `None` is a physical removal. MVCC delete stamps remain encoded records.
/// A host freezes this map by ownership transfer, not by cloning its payloads.
pub type FrozenOverlay = BTreeMap<EntityId, Option<Arc<[u8]>>>;

pub struct MergedCollection<'a> {
    base: &'a dyn CollectionRead,
    overlay: Arc<FrozenOverlay>,
}

impl<'a> MergedCollection<'a> {
    pub fn new(base: &'a dyn CollectionRead, overlay: Arc<FrozenOverlay>) -> Self {
        Self { base, overlay }
    }
}

impl CollectionRead for MergedCollection<'_> {
    fn get_record(
        &self,
        id: EntityId,
        limit_bytes: usize,
    ) -> Result<Option<EncodedRecord>, StoreError> {
        match self.overlay.get(&id) {
            Some(Some(bytes)) => {
                check_budget(bytes.len(), limit_bytes)?;
                Ok(Some(EncodedRecord {
                    id,
                    bytes: bytes.to_vec(),
                }))
            }
            Some(None) => Ok(None),
            None => self.base.get_record(id, limit_bytes),
        }
    }

    fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
        Ok(Box::new(MergedCursor {
            base: self.base.cursor()?,
            overlay: self.overlay.iter().peekable(),
        }))
    }

    fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
        let next = self
            .overlay
            .first_key_value()
            .map(|(id, bytes)| (*id, bytes.clone()));
        Ok(Box::new(OwnedMergedCursor {
            base: self.base.owned_cursor()?,
            overlay: Arc::clone(&self.overlay),
            next,
        }))
    }
    fn owned_key_cursor(&self, key: &str) -> Result<Option<Box<dyn CollectionCursor>>, StoreError> {
        let Some(base) = self.base.owned_key_cursor(key)? else {
            return Ok(None);
        };
        let next = self
            .overlay
            .first_key_value()
            .map(|(id, bytes)| (*id, bytes.clone()));
        Ok(Some(Box::new(OwnedMergedCursor {
            base,
            overlay: Arc::clone(&self.overlay),
            next,
        })))
    }
}

struct MergedCursor<'a> {
    base: Box<dyn CollectionCursor + 'a>,
    overlay:
        std::iter::Peekable<std::collections::btree_map::Iter<'a, EntityId, Option<Arc<[u8]>>>>,
}

impl CollectionCursor for MergedCursor<'_> {
    fn peek_id(&mut self) -> Result<Option<EntityId>, StoreError> {
        loop {
            let base = self.base.peek_id()?;
            let Some((id, bytes)) = self.overlay.peek() else {
                return Ok(base);
            };
            if base.is_some_and(|base| base < **id) {
                return Ok(base);
            }
            if bytes.is_some() {
                return Ok(Some(**id));
            }
            if base == Some(**id) {
                self.base.skip_record()?;
            }
            self.overlay.next();
        }
    }

    fn next_record(&mut self, limit_bytes: usize) -> Result<Option<EncodedRecord>, StoreError> {
        let Some(id) = self.peek_id()? else {
            return Ok(None);
        };
        if let Some((changed, Some(bytes))) = self.overlay.peek() {
            if **changed == id {
                check_budget(bytes.len(), limit_bytes)?;
                if self.base.peek_id()? == Some(id) {
                    self.base.skip_record()?;
                }
                let record = EncodedRecord {
                    id,
                    bytes: bytes.to_vec(),
                };
                self.overlay.next();
                return Ok(Some(record));
            }
        }
        self.base.next_record(limit_bytes)
    }

    fn skip_record(&mut self) -> Result<bool, StoreError> {
        let Some(id) = self.peek_id()? else {
            return Ok(false);
        };
        if self
            .overlay
            .peek()
            .is_some_and(|(changed, _)| **changed == id)
        {
            if self.base.peek_id()? == Some(id) {
                self.base.skip_record()?;
            }
            self.overlay.next();
            return Ok(true);
        }
        self.base.skip_record()
    }
}

struct OwnedMergedCursor {
    base: Box<dyn CollectionCursor>,
    overlay: Arc<FrozenOverlay>,
    next: Option<(EntityId, Option<Arc<[u8]>>)>,
}

impl OwnedMergedCursor {
    fn advance(&mut self) {
        let Some((id, _)) = self.next.take() else {
            return;
        };
        self.next = self
            .overlay
            .range((std::ops::Bound::Excluded(id), std::ops::Bound::Unbounded))
            .next()
            .map(|(id, bytes)| (*id, bytes.clone()));
    }
}

impl CollectionCursor for OwnedMergedCursor {
    fn peek_id(&mut self) -> Result<Option<EntityId>, StoreError> {
        loop {
            let base = self.base.peek_id()?;
            let Some((id, bytes)) = &self.next else {
                return Ok(base);
            };
            if base.is_some_and(|base| base < *id) {
                return Ok(base);
            }
            if bytes.is_some() {
                return Ok(Some(*id));
            }
            if base == Some(*id) {
                self.base.skip_record()?;
            }
            self.advance();
        }
    }
    fn next_record(&mut self, limit_bytes: usize) -> Result<Option<EncodedRecord>, StoreError> {
        let Some(id) = self.peek_id()? else {
            return Ok(None);
        };
        if let Some((changed, Some(bytes))) = &self.next {
            if *changed == id {
                check_budget(bytes.len(), limit_bytes)?;
                if self.base.peek_id()? == Some(id) {
                    self.base.skip_record()?;
                }
                let record = EncodedRecord {
                    id,
                    bytes: bytes.to_vec(),
                };
                self.advance();
                return Ok(Some(record));
            }
        }
        self.base.next_record(limit_bytes)
    }
    fn skip_record(&mut self) -> Result<bool, StoreError> {
        let Some(id) = self.peek_id()? else {
            return Ok(false);
        };
        if self
            .next
            .as_ref()
            .is_some_and(|(changed, _)| *changed == id)
        {
            if self.base.peek_id()? == Some(id) {
                self.base.skip_record()?;
            }
            self.advance();
            return Ok(true);
        }
        self.base.skip_record()
    }
}

fn physical_id(key: &[u8]) -> Result<EntityId, StoreError> {
    let bytes: [u8; 8] = key.try_into().map_err(|_| {
        StoreError::StorageIntegrity(StorageIntegrityError::new(
            "collection-key",
            "unknown",
            None,
            "physical entity key must contain eight bytes",
        ))
    })?;
    Ok(EntityId::new(u64::from_be_bytes(bytes)))
}

pub(super) fn read_error(error: BTreeError) -> StoreError {
    match error {
        BTreeError::ReadLimitExceeded {
            required_bytes,
            limit_bytes,
        } => StoreError::ReadLimitExceeded {
            required_bytes,
            limit_bytes,
        },
        BTreeError::Pager(message) => StoreError::Io(std::io::Error::other(message)),
        other => StoreError::StorageIntegrity(StorageIntegrityError::new(
            "collection-tree",
            "unknown",
            None,
            other.to_string(),
        )),
    }
}

fn check_budget(required_bytes: usize, limit_bytes: usize) -> Result<(), StoreError> {
    if required_bytes > limit_bytes {
        return Err(StoreError::ReadLimitExceeded {
            required_bytes,
            limit_bytes,
        });
    }
    Ok(())
}
