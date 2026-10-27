use super::{BorrowedMemoryFile, MemoryFile};
use reddb_engine::storage::{
    btree::BTree,
    page::{Page, PageType},
    page_store::PageStore,
    pager::{NoWal, Pager, PagerError},
    pager_io,
};
use std::{cell::Cell, sync::Arc};

#[path = "btree_restart.rs"]
mod restart;

struct FailLeafWrite {
    pager: Arc<Pager<MemoryFile, NoWal>>,
    fail: Cell<bool>,
}

impl PageStore for FailLeafWrite {
    fn read_page(&self, id: u32) -> Result<Page, PagerError> {
        self.pager.read_page(id)
    }
    fn write_page(&self, id: u32, page: Page) -> Result<(), PagerError> {
        if self.fail.get() && matches!(page.page_type(), Ok(PageType::BTreeLeaf)) {
            self.fail.set(false);
            return Err(PagerError::Io(std::io::Error::other(
                "injected leaf write failure",
            )));
        }
        self.pager.write_page(id, page)
    }
    fn allocate_page(&self, kind: PageType) -> Result<Page, PagerError> {
        self.pager.allocate_page(kind)
    }
    fn free_page(&self, id: u32) -> Result<(), PagerError> {
        self.pager.free_page(id)
    }
    fn page_count(&self) -> Result<u32, PagerError> {
        self.pager.page_count()
    }
}

pub fn btree_failed_delete_keeps_referenced_overflow_owned() {
    for operation in ["batch", "upsert", "delete"] {
        for deferred in [true, false] {
            let pager = restart::open(MemoryFile::default(), true).expect("pager");
            if deferred {
                pager
                    .enable_deferred_reclamation()
                    .expect("deferred reclamation");
            }
            let pages = Arc::new(FailLeafWrite {
                pager: pager.clone(),
                fail: Cell::new(false),
            });
            let tree = BTree::new(pages.clone());
            let mut state = 0x1234_5678_9abc_def0_u64;
            let payload: Vec<u8> = (0..24_000)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect();
            tree.insert(b"large", &payload).expect("overflow payload");
            if operation == "batch" {
                tree.insert(b"large2", &payload)
                    .expect("second overflow payload");
            }
            pager.sync().expect("persist old value");
            let before = pager.free_page_count().expect("free count");
            let before_pending = pager.deferred_reclamation_pages().expect("pending pages").0;
            pages.fail.set(true);
            let result = match operation {
                "delete" => tree.delete(b"large").map(|_| ()),
                "upsert" => tree.upsert(b"large", b"replacement"),
                "batch" => tree.upsert_batch_sorted(&[
                    (b"large".to_vec(), b"replacement".to_vec()),
                    (b"large2".to_vec(), b"replacement".to_vec()),
                ]),
                _ => unreachable!(),
            };
            assert!(
                result.is_err(),
                "leaf failure must be observable; operation={operation}"
            );
            assert_eq!(
                tree.get(b"large").expect("original remains readable"),
                Some(payload.clone())
            );
            if operation == "batch" {
                assert_eq!(
                    tree.get(b"large2")
                        .expect("second original remains readable"),
                    Some(payload.clone())
                );
            }
            assert_eq!(pager.free_page_count().expect("free count"), before, "failed mutation cannot free live overflow pages; operation={operation}, deferred={deferred}");
            assert_eq!(pager.deferred_reclamation_pages().expect("pending pages").0, before_pending, "failed mutation cannot retire live overflow pages; operation={operation}, deferred={deferred}");
            let unrelated = pages
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate unrelated page");
            pages
                .write_page(unrelated.page_id(), unrelated)
                .expect("write unrelated page");
            assert_eq!(
                tree.get(b"large").expect("old value after allocation"),
                Some(payload)
            );
            pager.sync().expect("sync after failed mutation");
            let bytes = pager
                .with_file(|file| file.durable.borrow().clone())
                .expect("durable copy");
            let reopened =
                restart::open(MemoryFile::from_bytes(bytes), false).expect("reopen durable copy");
            let recovered = BTree::with_root(reopened, tree.root_page_id());
            assert_eq!(
                recovered.get(b"large").expect("recovered value"),
                tree.get(b"large").expect("live value")
            );
            if operation == "batch" {
                assert_eq!(
                    recovered.get(b"large2").expect("second recovered value"),
                    tree.get(b"large2").expect("second live value")
                );
            }
            match operation {
                "delete" => {
                    assert!(tree.delete(b"large").expect("successful delete"));
                }
                "upsert" => tree
                    .upsert(b"large", b"replacement")
                    .expect("successful upsert"),
                "batch" => tree
                    .upsert_batch_sorted(&[
                        (b"large".to_vec(), b"replacement".to_vec()),
                        (b"large2".to_vec(), b"replacement".to_vec()),
                    ])
                    .expect("successful batch"),
                _ => unreachable!(),
            }
            assert_eq!(
                tree.get(b"large").expect("read after success"),
                if operation == "delete" {
                    None
                } else {
                    Some(b"replacement".to_vec())
                }
            );
            if operation == "batch" {
                assert_eq!(
                    tree.get(b"large2").expect("second updated row"),
                    Some(b"replacement".to_vec())
                );
            }
            if deferred {
                assert!(
                    pager
                        .deferred_reclamation_pages()
                        .expect("pending pages")
                        .0
                        .len()
                        > before_pending.len(),
                    "successful mutation retires its old overflow pages"
                );
            } else {
                assert!(
                    pager.free_page_count().expect("free count") > before,
                    "successful mutation releases its old overflow pages"
                );
            }
        }
    }
}

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
