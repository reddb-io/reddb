//! Test-only persisted B-tree records, shared by the native and OPFS fixtures.
//! These opaque records exercise the tree; they are not runtime entities or transactions.
use reddb_engine::storage::{
    btree::{BTree, BTreeError},
    page::{Page, PageType},
    page_store::PageStore,
    pager::{NoWal, Pager, PagerConfig, PagerError},
};
use reddb_file::VfsFile;
use std::{collections::BTreeMap, sync::Arc};

pub const RECORD_COUNT: u32 = 600;
const COLLECTION: &str = "browser-btree-fixture";

pub fn value(id: u32) -> Vec<u8> {
    let size = if id.is_multiple_of(9) { 1024 } else { 96 };
    bytes(id.wrapping_add(1), size)
}

pub fn bytes(mut seed: u32, count: usize) -> Vec<u8> {
    (0..count)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed.to_le_bytes()[0]
        })
        .collect()
}

pub fn expected(updated: bool) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut records: BTreeMap<_, _> = (0..RECORD_COUNT)
        .map(|id| (id.to_be_bytes().to_vec(), value(id)))
        .collect();
    records.insert(b"compressed".to_vec(), vec![b'z'; 100_000]);
    records.insert(b"overflow".to_vec(), bytes(7, 70_000));
    if updated {
        for id in 50..150u32 {
            records.remove(id.to_be_bytes().as_slice());
        }
        for id in 0..30u32 {
            records.insert(id.to_be_bytes().to_vec(), bytes(id + 1000, 40));
        }
        for id in 700..720u32 {
            records.insert(id.to_be_bytes().to_vec(), value(id));
        }
        records.insert(b"overflow".to_vec(), bytes(9, 90_000));
    }
    records
}

pub fn seed<P: PageStore>(tree: &BTree<P>) -> Result<(), BTreeError> {
    let records: Vec<_> = expected(false).into_iter().collect();
    tree.bulk_insert_sorted(&records)
}

pub fn verify<P: PageStore>(tree: &BTree<P>, updated: bool) -> Result<(), BTreeError> {
    let records = expected(updated);
    assert_eq!(tree.count()?, records.len());
    for (key, value) in &records {
        assert_eq!(tree.get(key)?.as_ref(), Some(value), "point lookup {key:?}");
    }
    let mut cursor = tree.cursor_first()?;
    for entry in records {
        if let Some(peeked) = cursor.peek()? {
            assert_eq!(peeked, entry);
        }
        assert_eq!(cursor.next()?, Some(entry));
    }
    assert!(cursor.next()?.is_none());
    let mut cursor = tree.cursor_seek(&200u32.to_be_bytes())?;
    assert_eq!(cursor.next()?.expect("seek result").0, 200u32.to_be_bytes());
    Ok(())
}

pub fn mutate<P: PageStore>(tree: &BTree<P>) -> Result<(), BTreeError> {
    for id in 50..150u32 {
        assert!(tree.delete(&id.to_be_bytes())?);
    }
    let updates: Vec<_> = (0..30u32)
        .map(|id| (id.to_be_bytes().to_vec(), bytes(id + 1000, 40)))
        .collect();
    tree.upsert_batch_sorted(&updates)?;
    for id in 700..720u32 {
        tree.insert(&id.to_be_bytes(), &value(id))?;
    }
    tree.upsert(b"overflow", &bytes(9, 90_000))?;
    verify(tree, true)
}

/// A single test collection root, using the canonical native metadata codecs.
/// Callers sync all children before this root, then sync the root itself.
pub fn publish_root<P: PageStore>(pager: &P, root: u32) -> Result<(), PagerError> {
    let mut metadata = Vec::new();
    reddb_file::encode_native_paged_metadata_header(
        &mut metadata,
        reddb_file::NativePagedMetadataHeader {
            format_version: reddb_file::STORE_VERSION_CURRENT,
            collection_count: 1,
        },
    );
    reddb_file::encode_native_paged_collection_root(&mut metadata, COLLECTION, root);
    let mut page = Page::new(PageType::Header, 1);
    page.content_mut()[..metadata.len()].copy_from_slice(&metadata);
    pager.write_page(1, page)
}

pub fn read_root<P: PageStore>(pager: &P) -> Result<u32, PagerError> {
    let page = pager.read_page(1)?;
    let content = page.content();
    let header = reddb_file::decode_native_paged_metadata_header(content)
        .map_err(|error| PagerError::InvalidDatabase(error.to_string()))?
        .ok_or_else(|| PagerError::InvalidDatabase("fixture metadata header missing".into()))?;
    assert_eq!(header.collection_count, 1);
    let mut position = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(content, &mut position)
        .map_err(|error| PagerError::InvalidDatabase(error.to_string()))?;
    assert_eq!(root.collection, COLLECTION);
    Ok(root.root_page)
}

pub fn open<F: VfsFile>(file: F, create: bool) -> Result<Arc<Pager<F, NoWal>>, PagerError> {
    Pager::open_owned(
        file,
        "btree-fixture.rdb".into(),
        PagerConfig {
            cache_size: 2,
            ..PagerConfig::default()
        },
        NoWal,
        create,
    )
    .map(Arc::new)
}

pub fn run<F: VfsFile>(file: F, phase: u32) -> Result<(), Box<dyn std::error::Error>> {
    let pager = open(file, phase == 0)?;
    if phase == 0 {
        let tree = BTree::new(Arc::clone(&pager));
        seed(&tree)?;
        pager.sync()?;
        publish_root(pager.as_ref(), tree.root_page_id())?;
        pager.sync()?;
    } else {
        let root = read_root(pager.as_ref())?;
        let tree = BTree::with_root(Arc::clone(&pager), root);
        match phase {
            1 => {
                verify(&tree, false)?;
                mutate(&tree)?;
                pager.sync()?;
                publish_root(pager.as_ref(), tree.root_page_id())?;
                pager.sync()?;
            }
            2 => verify(&tree, true)?,
            3 => verify(&tree, false)?,
            _ => return Err("invalid B-tree fixture phase".into()),
        }
    }
    Ok(())
}
