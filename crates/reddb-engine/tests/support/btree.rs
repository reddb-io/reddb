use super::{BorrowedMemoryFile, MemoryFile};
use reddb_engine::storage::{btree::BTree, page::PageType, pager_io};
use std::sync::Arc;

#[path = "btree_restart.rs"]
mod restart;

pub fn btree_persisted_round_trip() {
    let pager = restart::open(MemoryFile::default(), true).expect("create pager");
    let tree = BTree::new(Arc::clone(&pager));
    restart::seed(&tree).expect("seed tree with mixed-width, compressed and overflow values");
    restart::verify(&tree, false).expect("tree matches independent map");
    assert_eq!(
        pager
            .read_page(tree.root_page_id())
            .expect("root")
            .page_type()
            .expect("type"),
        PageType::BTreeInterior
    );
    pager.sync().expect("persist child pages");
    restart::publish_root(pager.as_ref(), tree.root_page_id()).expect("publish canonical root");
    pager.sync().expect("persist root");
    let bytes = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("snapshot");
    let mut file = MemoryFile::from_bytes(bytes);
    restart::run(BorrowedMemoryFile(&mut file), 1).expect("mutate through fresh pager");
    let bytes = file.durable.into_inner();
    restart::run(MemoryFile::from_bytes(bytes), 2).expect("mutations survive another restart");
}

pub fn btree_reclamation_and_cursor_lifetime() {
    let pager = restart::open(MemoryFile::default(), true).expect("create pager");
    pager
        .enable_deferred_reclamation()
        .expect("store-managed reclamation");
    let tree = BTree::new(Arc::clone(&pager));
    restart::seed(&tree).expect("seed");
    let mut cursor = tree.cursor_first().expect("cursor");
    assert!(tree.has_retirement_readers());
    let pages = tree
        .retired_pages()
        .expect("validate retired tree ownership");
    assert!(
        pages.len() > 5,
        "includes interior, leaf and overflow pages"
    );
    drop(tree);
    assert!(cursor
        .next()
        .expect("cursor outlives tree handle")
        .is_some());
    drop(cursor);
    pager
        .free_retired_pages(&pages)
        .expect("release validated unpublished generation");
    assert_eq!(
        pager.free_page_count().expect("free count") as usize,
        pages.len()
    );
    pager.sync().expect("persist free ownership");
}

pub fn btree_bounds_and_corruption() {
    let pager = restart::open(MemoryFile::default(), true).expect("create pager");
    let tree = BTree::new(Arc::clone(&pager));
    assert!(tree
        .insert(
            &vec![1; reddb_engine::storage::btree::MAX_KEY_SIZE + 1],
            b"v"
        )
        .is_err());
    assert_eq!(tree.count().expect("invalid key has no effect"), 0);
    tree.insert(b"k", b"v").expect("insert");
    assert!(tree.insert(b"k", b"different").is_err());
    assert_eq!(
        tree.get(b"k").expect("read after rejection"),
        Some(b"v".to_vec())
    );
    assert!(!tree.delete(b"missing").expect("delete absent key"));
    // A checksummed but wrong page type is rejected by the cursor.
    let wrong = reddb_engine::storage::page::Page::new(PageType::Header, tree.root_page_id());
    pager
        .write_page(tree.root_page_id(), wrong)
        .expect("inject wrong type");
    assert!(tree.cursor_first().is_err());
    let bad = BTree::with_root(Arc::clone(&pager), pager_io::FIRST_ALLOCATABLE_PAGE + 100);
    assert!(bad.get(b"k").is_err());
}
