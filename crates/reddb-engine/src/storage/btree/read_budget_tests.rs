use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Mutex, Weak};

#[derive(Default)]
struct PublishingPages {
    next: AtomicU32,
    pages: Mutex<HashMap<u32, Page>>,
    reads: AtomicUsize,
    overflow_reads: AtomicUsize,
    publication: Mutex<Option<(Weak<BTree<Self>>, u32)>>,
}

impl PageStore for PublishingPages {
    fn read_page(&self, id: u32) -> Result<Page, PagerError> {
        // One initial descent, then 32 right-link reads. Publish the current
        // root at the final hop so the retry must descend through that root.
        if self.reads.fetch_add(1, AtomicOrdering::Relaxed) == 32 {
            if let Some((tree, root)) = self.publication.lock().expect("publication").take() {
                *tree
                    .upgrade()
                    .expect("live tree")
                    .root_page_id
                    .write()
                    .expect("root") = root;
            }
        }
        let page = self.pages.lock().expect("pages")[&id].clone();
        if page.page_type().expect("page type") == PageType::Overflow {
            self.overflow_reads.fetch_add(1, AtomicOrdering::Relaxed);
        }
        Ok(page)
    }

    fn write_page(&self, id: u32, page: Page) -> Result<(), PagerError> {
        self.pages.lock().expect("pages").insert(id, page);
        Ok(())
    }

    fn allocate_page(&self, kind: PageType) -> Result<Page, PagerError> {
        let id = self.next.fetch_add(1, AtomicOrdering::Relaxed) + 1;
        let page = Page::new(kind, id);
        self.write_page(id, page.clone())?;
        Ok(page)
    }

    fn free_page(&self, id: u32) -> Result<(), PagerError> {
        self.pages.lock().expect("pages").remove(&id);
        Ok(())
    }

    fn page_count(&self) -> Result<u32, PagerError> {
        Ok(self.next.load(AtomicOrdering::Relaxed) + 1)
    }
}

fn noise(length: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_u32;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn root_retry_preserves_read_budget_before_overflow_or_decompression() {
    for payload in [noise(32 * 1024), vec![b'x'; 32 * 1024]] {
        let pages = Arc::new(PublishingPages::default());
        let tree = Arc::new(BTree::new(Arc::clone(&pages)));
        let row = noise(1024);
        for key in 0..640_u64 {
            tree.insert(&key.to_be_bytes(), &row).expect("seed leaves");
        }
        let target = u64::MAX.to_be_bytes();
        tree.insert(&target, &payload).expect("large target");
        let current_root = tree.root_page_id();
        let first_leaf = tree.find_first_leaf(current_root).expect("first leaf");
        let leaf_count = pages
            .pages
            .lock()
            .expect("pages")
            .values()
            .filter(|page| page.page_type().expect("page type") == PageType::BTreeLeaf)
            .count();
        assert!(
            leaf_count > 33,
            "target must be beyond the right-link hop cap"
        );

        let arm_retry = || {
            *tree.root_page_id.write().expect("root") = first_leaf;
            *pages.publication.lock().expect("publication") =
                Some((Arc::downgrade(&tree), current_root));
            pages.reads.store(0, AtomicOrdering::Relaxed);
            pages.overflow_reads.store(0, AtomicOrdering::Relaxed);
        };
        arm_retry();
        let result = tree.get_with_budget(&target, 512);
        assert!(pages.publication.lock().expect("publication").is_none());
        assert_eq!(tree.root_page_id(), current_root);
        assert!(
            matches!(
                result,
                Err(BTreeError::ReadLimitExceeded {
                    limit_bytes: 512,
                    ..
                })
            ),
            "root retry must reject a 32 KiB value under a 512-byte allowance"
        );
        assert_eq!(pages.overflow_reads.load(AtomicOrdering::Relaxed), 0);

        arm_retry();
        assert_eq!(
            tree.get_with_budget(&target, 64 * 1024)
                .expect("admitted retry"),
            Some(payload)
        );
        assert!(pages.publication.lock().expect("publication").is_none());
    }
}
