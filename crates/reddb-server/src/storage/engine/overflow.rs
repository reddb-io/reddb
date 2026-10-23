//! Native compatibility paths for the shared overflow-chain implementation.
pub use reddb_engine::storage::overflow::{
    pages_needed, OverflowError, OVERFLOW_PAGE_HEADER, OVERFLOW_PAYLOAD_PER_PAGE,
};
pub type OverflowChain<'p> =
    reddb_engine::storage::overflow::OverflowChain<'p, super::pager::Pager>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::pager::Pager;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_db_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "reddb_overflow_test_{}_{}.db",
            std::process::id(),
            id
        ));
        path
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        for sidecar in reddb_file::layout::pager_shadow_sidecar_paths(path) {
            let _ = std::fs::remove_file(sidecar);
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + 7) & 0xff) as u8).collect()
    }

    #[test]
    fn pages_needed_boundaries() {
        assert_eq!(pages_needed(0), 1);
        assert_eq!(pages_needed(1), 1);
        assert_eq!(pages_needed(OVERFLOW_PAYLOAD_PER_PAGE), 1);
        assert_eq!(pages_needed(OVERFLOW_PAYLOAD_PER_PAGE + 1), 2);
        assert_eq!(pages_needed(OVERFLOW_PAYLOAD_PER_PAGE * 4), 4);
        assert_eq!(pages_needed(OVERFLOW_PAYLOAD_PER_PAGE * 4 + 1), 5);
    }

    fn roundtrip(pager: &Pager, len: usize) {
        let chain = OverflowChain::new(pager);
        let data = pattern(len);
        let (head, total) = chain.store(&data).unwrap();
        assert_eq!(total, len as u64);
        let read_back = chain.read(head, total).unwrap();
        assert_eq!(read_back.len(), len);
        assert_eq!(read_back, data);
        chain.free(head).unwrap();
    }

    #[test]
    fn store_read_roundtrips_across_sizes() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            // one page
            roundtrip(&pager, 1);
            roundtrip(&pager, 100);
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE - 1);
            // exact one-page boundary
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE);
            // two pages
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE + 1);
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE * 2);
            // several pages
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE * 5 + 123);
            // many pages
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE * 32);
            // exact multi-page boundary
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE * 7);
        }
        cleanup(&path);
    }

    #[test]
    fn store_empty_value_produces_single_page() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            let chain = OverflowChain::new(&pager);
            let pages_before = pager.page_count().unwrap();
            let (head, total) = chain.store(&[]).unwrap();
            assert_eq!(total, 0);
            let bytes = chain.read(head, total).unwrap();
            assert!(bytes.is_empty());

            assert_eq!(
                pager.page_count().unwrap(),
                pages_before + 1,
                "empty value allocates exactly one terminating head page"
            );

            chain.free(head).unwrap();
        }
        cleanup(&path);
    }

    #[test]
    fn read_with_wrong_total_len_errors() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            let chain = OverflowChain::new(&pager);
            let data = pattern(OVERFLOW_PAYLOAD_PER_PAGE * 3 + 50);
            let (head, total) = chain.store(&data).unwrap();

            // Too short: chain reports more bytes than caller expects.
            let err = chain.read(head, total - 10).unwrap_err();
            assert!(matches!(err, OverflowError::LengthMismatch { .. }));

            // Too long: chain ends before caller's expected length.
            let err = chain.read(head, total + 10).unwrap_err();
            assert!(matches!(err, OverflowError::LengthMismatch { .. }));

            chain.free(head).unwrap();
        }
        cleanup(&path);
    }

    #[test]
    fn free_returns_pages_to_freelist_observably() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            let chain = OverflowChain::new(&pager);

            let len = OVERFLOW_PAYLOAD_PER_PAGE * 6 + 17;
            let n = pages_needed(len);
            let data = pattern(len);

            let before_alloc = pager.page_count().unwrap();
            let (head, _) = chain.store(&data).unwrap();
            let after_alloc = pager.page_count().unwrap();
            assert_eq!((after_alloc - before_alloc) as usize, n);

            chain.free(head).unwrap();

            // A second store of the same size must reuse the freed pages
            // rather than extending the file.
            let after_free = pager.page_count().unwrap();
            let (head2, _) = chain.store(&data).unwrap();
            let after_realloc = pager.page_count().unwrap();
            assert_eq!(
                after_realloc, after_free,
                "second store should reuse freed pages"
            );

            chain.free(head2).unwrap();
        }
        cleanup(&path);
    }

    #[test]
    fn free_then_store_reuses_pages_exact_count() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            let chain = OverflowChain::new(&pager);

            let len = OVERFLOW_PAYLOAD_PER_PAGE * 4;
            let (head, _) = chain.store(&pattern(len)).unwrap();
            let baseline = pager.page_count().unwrap();
            chain.free(head).unwrap();
            // free does not shrink the file
            assert_eq!(pager.page_count().unwrap(), baseline);

            let (head2, _) = chain.store(&pattern(len)).unwrap();
            assert_eq!(pager.page_count().unwrap(), baseline);
            chain.free(head2).unwrap();
        }
        cleanup(&path);
    }
}
