//! Exclusive-owner table storage over the canonical pager, B-tree and RER1 codec.
//! Schemas use the canonical physical metadata document; SQL stays above this layer.
use super::{
    btree::BTree,
    collection_read::{CollectionCursor, PagedCollection},
    entity::{EntityData, EntityId, UnifiedEntity, FIRST_USER_ENTITY_ID},
    metadata_codec::{deserialize_entity_record_with_budget, serialize_entity_record},
    page::{Page, PageType, HEADER_SIZE, PAGE_SIZE},
    pager::{NoWal, Pager, PagerConfig},
};
use crate::catalog::CollectionContract;
use reddb_file::{VfsFile, STORE_VERSION_CURRENT};
use std::{collections::BTreeMap, io, sync::Arc};

mod catalog;

pub const RECORD_BYTES_MAX: usize = 8192;
const METADATA_COLLECTION: &str = "red.storage.metadata";
type FilePager<F> = Pager<F, NoWal>;

struct Collection<F: VfsFile> {
    tree: Arc<BTree<FilePager<F>>>,
    count: u64,
    last_id: u64,
    contract: Option<CollectionContract>,
}

pub struct PagedFileStore<F: VfsFile> {
    pager: Arc<FilePager<F>>,
    collections: BTreeMap<String, Collection<F>>,
    next_id: u64,
    failed: bool,
}

fn error(cause: impl std::fmt::Display) -> io::Error {
    io::Error::other(cause.to_string())
}

impl<F: VfsFile> PagedFileStore<F> {
    pub fn create(file: F) -> io::Result<Self> {
        Self::open_inner(file, true)
    }

    pub fn open(file: F) -> io::Result<Self> {
        Self::open_inner(file, false)
    }

    fn open_inner(file: F, create: bool) -> io::Result<Self> {
        let pager = Self::open_pager(file, create)?;
        let mut store = Self {
            pager,
            collections: BTreeMap::new(),
            next_id: FIRST_USER_ENTITY_ID,
            failed: false,
        };
        if create {
            store.pager.begin_write_batch().map_err(error)?;
            store.publish_catalog()?;
            store.pager.commit_write_batch().map_err(error)?;
            return Ok(store);
        }
        for root in store.read_roots()? {
            store.restore_collection(root)?;
        }
        store.load_contracts()?;
        Ok(store)
    }

    #[inline(never)]
    fn open_pager(file: F, create: bool) -> io::Result<Arc<FilePager<F>>> {
        Pager::open_owned(
            file,
            "database.rdb".into(),
            PagerConfig {
                cache_size: 1,
                ..Default::default()
            },
            NoWal,
            create,
        )
        .map(Arc::new)
        .map_err(error)
    }

    #[inline(never)]
    fn read_roots(&self) -> io::Result<Vec<reddb_file::NativePagedCollectionRoot>> {
        let page = self.pager.read_manifest_page().map_err(error)?;
        if page.page_type().map_err(error)? != PageType::Header {
            return Err(error("invalid native catalog page"));
        }
        let content = &page.as_bytes()[HEADER_SIZE..];
        let header = reddb_file::decode_native_paged_metadata_header(content)
            .map_err(error)?
            .ok_or_else(|| error("native collection roots missing"))?;
        if header.format_version != STORE_VERSION_CURRENT
            || header.collection_count as usize > content.len() / 8
        {
            return Err(error("native catalog exceeds local admission"));
        }
        let mut position = reddb_file::METADATA_HEADER_BYTES;
        let mut roots = Vec::with_capacity(header.collection_count as usize);
        for _ in 0..header.collection_count {
            roots.push(
                reddb_file::decode_native_paged_collection_root(content, &mut position)
                    .map_err(error)?,
            );
        }
        let reference_count = content
            .get(position..position + 4)
            .ok_or_else(|| error("native catalog reference count is missing"))?;
        position += 4;
        if u32::from_le_bytes(reference_count.try_into().expect("four bytes admitted")) != 0
            || content[position..].iter().any(|byte| *byte != 0)
        {
            return Err(error(
                "native catalog contains extensions this bounded writer cannot preserve",
            ));
        }
        Ok(roots)
    }

    #[inline(never)]
    fn restore_collection(
        &mut self,
        root: reddb_file::NativePagedCollectionRoot,
    ) -> io::Result<()> {
        let tree = Arc::new(BTree::with_root(Arc::clone(&self.pager), root.root_page));
        let collection = PagedCollection::new(Arc::clone(&tree));
        let mut cursor = collection.scan().map_err(error)?;
        let (mut count, mut last_id) = (0, 0);
        let decode_bytes = if root.collection == METADATA_COLLECTION {
            3 * RECORD_BYTES_MAX
        } else {
            RECORD_BYTES_MAX
        };
        while let Some(record) = cursor.next_record(RECORD_BYTES_MAX).map_err(error)? {
            let (entity, metadata) = record
                .decode_with_budget(STORE_VERSION_CURRENT, decode_bytes)
                .map_err(error)?;
            if entity.logical_id() != entity.id
                || entity.xmin != 0
                || entity.xmax != 0
                || metadata.is_some()
            {
                return Err(error(
                    "local file host requires committed rows without MVCC or entity metadata",
                ));
            }
            last_id = record.id.raw();
            count += 1;
        }
        self.next_id = self.next_id.max(
            last_id
                .checked_add(1)
                .ok_or_else(|| error("entity identity overflow"))?,
        );
        if self
            .collections
            .insert(
                root.collection,
                Collection {
                    tree,
                    count,
                    last_id,
                    contract: None,
                },
            )
            .is_some()
        {
            return Err(error("duplicate collection root"));
        }
        Ok(())
    }

    pub fn check_healthy(&self) -> io::Result<()> {
        if self.failed {
            Err(error("reopen after interrupted page publication"))
        } else {
            Ok(())
        }
    }

    pub fn next_entity_id(&self) -> io::Result<u64> {
        self.check_healthy()?;
        Ok(self.next_id)
    }

    pub fn contract(&self, name: &str) -> io::Result<&CollectionContract> {
        self.check_healthy()?;
        self.collections
            .get(name)
            .and_then(|state| state.contract.as_ref())
            .ok_or_else(|| error("collection has no declared contract"))
    }

    pub fn contains_collection(&self, name: &str) -> io::Result<bool> {
        self.check_healthy()?;
        Ok(self.collections.contains_key(name))
    }

    pub fn contracts(&self) -> impl Iterator<Item = &CollectionContract> {
        self.collections
            .values()
            .filter_map(|state| state.contract.as_ref())
    }

    pub fn count(&self) -> io::Result<u64> {
        self.check_healthy()?;
        Ok(self
            .collections
            .values()
            .filter(|state| state.contract.is_some())
            .map(|state| state.count)
            .sum())
    }

    pub fn collection(&self, name: &str) -> io::Result<PagedCollection<FilePager<F>>> {
        self.check_healthy()?;
        self.collections
            .get(name)
            .map(|state| PagedCollection::new(Arc::clone(&state.tree)))
            .ok_or_else(|| error("collection missing"))
    }

    pub fn cache_pages(&self) -> usize {
        self.pager.cache_capacity()
    }

    pub fn into_inner(mut self) -> F {
        self.collections.clear();
        match Arc::try_unwrap(self.pager) {
            Ok(pager) => pager.into_inner(),
            Err(_) => panic!("collection handles must not outlive their paged store"),
        }
    }

    pub fn register(&mut self, contract: CollectionContract, now_ms: u64) -> io::Result<()> {
        self.check_healthy()?;
        if self.collections.contains_key(&contract.name) {
            return Err(error("collection already exists"));
        }
        let document = self.contract_document(Some(&contract), now_ms)?;
        if document.len() > RECORD_BYTES_MAX - 1024 {
            return Err(error("catalog document exceeds resource budget"));
        }
        let required_capabilities =
            crate::storage::entity_codec::required_file_capabilities_for_contract(&contract);
        if required_capabilities != 0 {
            self.pager
                .fence_required_capabilities(required_capabilities)
                .map_err(error)?;
        }
        self.failed = true;
        let result = (|| {
            self.pager.begin_write_batch().map_err(error)?;
            self.collections.insert(
                contract.name.clone(),
                Collection {
                    tree: Arc::new(BTree::new(Arc::clone(&self.pager))),
                    count: 0,
                    last_id: 0,
                    contract: Some(contract),
                },
            );
            self.store_contract_document(document, now_ms)?;
            self.publish_catalog()?;
            self.pager.commit_write_batch().map_err(error)
        })();
        if result.is_err() {
            self.pager.fence_write_batch();
        } else {
            self.failed = false;
        }
        result
    }

    /// Records are canonical serialized entities admitted before any I/O.
    /// One prepared native DWB frame commits rows and catalog roots together.
    pub fn insert_records(
        &mut self,
        name: &str,
        records: &[(u64, Vec<u8>)],
        required_capabilities: u64,
    ) -> io::Result<()> {
        self.check_healthy()?;
        if !self.collections.contains_key(name) {
            return Err(error("collection missing"));
        }
        for (offset, (id, record)) in records.iter().enumerate() {
            if Some(*id) != self.next_id.checked_add(offset as u64)
                || record.len() > RECORD_BYTES_MAX
            {
                return Err(error("native record admission failed"));
            }
        }
        let next = self
            .next_id
            .checked_add(records.len() as u64)
            .filter(|id| *id < u64::MAX)
            .ok_or_else(|| error("entity identity overflow"))?;
        if required_capabilities != 0 {
            self.pager
                .fence_required_capabilities(required_capabilities)
                .map_err(error)?;
        }
        self.failed = true;
        let result = (|| {
            self.pager.begin_write_batch().map_err(error)?;
            let state = self.collections.get_mut(name).expect("collection admitted");
            let root = state.tree.root_page_id();
            for (id, record) in records {
                state
                    .tree
                    .insert(&id.to_be_bytes(), record)
                    .map_err(error)?;
            }
            state.count += records.len() as u64;
            state.last_id = records.last().map_or(state.last_id, |(id, _)| *id);
            self.next_id = next;
            if state.tree.root_page_id() != root {
                self.publish_catalog()?;
            }
            self.pager.commit_write_batch().map_err(error)
        })();
        if result.is_err() {
            self.pager.fence_write_batch();
        } else {
            self.failed = false;
        }
        result
    }

    #[inline(never)]
    fn publish_catalog(&self) -> io::Result<()> {
        let mut bytes = Vec::new();
        reddb_file::encode_native_paged_metadata_header(
            &mut bytes,
            reddb_file::NativePagedMetadataHeader {
                format_version: STORE_VERSION_CURRENT,
                collection_count: u32::try_from(self.collections.len())
                    .map_err(|_| error("native catalog exceeds local admission"))?,
            },
        );
        for (name, state) in &self.collections {
            reddb_file::encode_native_paged_collection_root(
                &mut bytes,
                name,
                state.tree.root_page_id(),
            );
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        if bytes.len() > PAGE_SIZE - HEADER_SIZE {
            return Err(error("native catalog exceeds resource budget"));
        }
        let mut page = Page::new(PageType::Header, 1);
        page.as_bytes_mut()[HEADER_SIZE..HEADER_SIZE + bytes.len()].copy_from_slice(&bytes);
        self.pager.write_page(1, page).map_err(error)
    }

    #[inline(never)]
    fn load_contracts(&mut self) -> io::Result<()> {
        let Some(state) = self.collections.get(METADATA_COLLECTION) else {
            if self.collections.is_empty() {
                return Ok(());
            }
            return Err(error("physical metadata document missing"));
        };
        let bytes = state
            .tree
            .get_with_budget(&state.last_id.to_be_bytes(), RECORD_BYTES_MAX)
            .map_err(error)?
            .ok_or_else(|| error("physical metadata record missing"))?;
        let mut budget = reddb_file::native_value::DecodeBudget::new(3 * RECORD_BYTES_MAX);
        let (entity, _) =
            deserialize_entity_record_with_budget(&bytes, STORE_VERSION_CURRENT, &mut budget)
                .map_err(error)?;
        drop(bytes);
        let EntityData::Row(row) = entity.data else {
            return Err(error("invalid physical metadata row"));
        };
        let Some(reddb_types::Value::Text(document)) = row.get_field("value") else {
            return Err(error("invalid physical metadata document"));
        };
        let envelope =
            reddb_file::physical_metadata::decode_physical_metadata_document_root_json(document)
                .map_err(error)?;
        drop(row);
        for json in envelope.collection_contracts_json {
            let persisted =
                reddb_file::physical_metadata::decode_physical_collection_contract_json(&json)
                    .map_err(error)?;
            let contract = crate::catalog::codec::collection_contract_from_persisted(persisted)
                .map_err(error)?;
            let state = self
                .collections
                .get_mut(&contract.name)
                .ok_or_else(|| error("contract root missing"))?;
            if state.contract.replace(contract).is_some() {
                return Err(error("duplicate contract"));
            }
        }
        if self
            .collections
            .iter()
            .any(|(name, state)| name != METADATA_COLLECTION && state.contract.is_none())
        {
            return Err(error("collection contract missing"));
        }
        Ok(())
    }

    fn store_contract_document(&mut self, document: String, now_ms: u64) -> io::Result<()> {
        let state = self
            .collections
            .entry(METADATA_COLLECTION.into())
            .or_insert_with(|| Collection {
                tree: Arc::new(BTree::new(Arc::clone(&self.pager))),
                count: 0,
                last_id: 0,
                contract: None,
            });
        let id = state.count + 1;
        if id >= FIRST_USER_ENTITY_ID {
            return Err(error("reserved metadata identity budget"));
        }
        let mut entity =
            UnifiedEntity::table_row(EntityId::new(id), METADATA_COLLECTION, id, Vec::new());
        entity.data = EntityData::Row(super::entity::RowData::with_names(
            vec![
                reddb_types::Value::text("physical"),
                reddb_types::Value::text(document),
            ],
            vec!["key".into(), "value".into()],
        ));
        entity.created_at = now_ms / 1000;
        entity.updated_at = now_ms / 1000;
        entity.sequence_id = id;
        entity.field_bloom = super::entity::compute_entity_field_bloom(&entity.data);
        entity.ensure_table_logical_id();
        let record = serialize_entity_record(&entity, None, STORE_VERSION_CURRENT);
        state
            .tree
            .insert(&id.to_be_bytes(), &record)
            .map_err(error)?;
        state.count = id;
        state.last_id = id;
        Ok(())
    }
}
