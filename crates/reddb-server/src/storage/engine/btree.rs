//! Native compatibility paths for the shared B-tree implementation.
use super::pager::Pager;
pub use reddb_engine::storage::btree::{
    lehman_yao, BTreeError, BTreeResult, MAX_KEY_SIZE, MAX_VALUE_SIZE, MIN_FILL_FACTOR,
};
#[cfg(test)]
use std::sync::Arc;
pub type BTree = reddb_engine::storage::btree::BTree<Pager>;
pub type BTreeCursor = reddb_engine::storage::btree::BTreeCursor<Pager>;
#[path = "btree/value_layout.rs"]
pub mod value_layout;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// RAII temp DB path: removes the `.db` and every pager sidecar
    /// (`.db-dwb`, `.db-hdr`, `.db-wal`, …) on drop so nothing is left in
    /// TMPDIR for the leak guard (scripts/check-temp-residue.sh). The existing
    /// tests only call `cleanup` at the START (a no-op for unique paths), so
    /// without this the sidecars created during the test leaked. Derefs to
    /// `PathBuf` so every `&path` / `cleanup(&path)` call site keeps working.
    struct TempDbPath(PathBuf);
    impl std::ops::Deref for TempDbPath {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }
    impl AsRef<std::path::Path> for TempDbPath {
        fn as_ref(&self) -> &std::path::Path {
            self.0.as_ref()
        }
    }
    impl Drop for TempDbPath {
        fn drop(&mut self) {
            cleanup(&self.0);
        }
    }

    fn temp_db_path() -> TempDbPath {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("reddb_btree_test_{}_{}.db", std::process::id(), id));
        TempDbPath(path)
    }

    fn cleanup(path: &PathBuf) {
        // Remove the `.db` AND every sidecar the pager writes next to it
        // (`.db-dwb`, `.db-hdr`, `.db-wal`, …) so nothing is left in TMPDIR for
        // the leak guard (scripts/check-temp-residue.sh) to flag.
        if let (Some(dir), Some(name)) = (
            path.parent(),
            path.file_name().and_then(|name| name.to_str()),
        ) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if entry
                        .file_name()
                        .to_str()
                        .is_some_and(|entry_name| entry_name.starts_with(name))
                    {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }

    #[test]
    fn test_btree_empty() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        assert!(tree.is_empty());
        assert_eq!(tree.root_page_id(), 0);
        assert_eq!(tree.get(b"key").expect("get() should succeed"), None);
        assert_eq!(tree.count().expect("count() should succeed"), 0);

        cleanup(&path);
    }

    #[test]
    fn test_btree_single_insert() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        tree.insert(b"hello", b"world")
            .expect("insert() should succeed");

        assert!(!tree.is_empty());
        assert_eq!(
            tree.get(b"hello").expect("get() should succeed"),
            Some(b"world".to_vec())
        );
        assert_eq!(tree.get(b"other").expect("get() should succeed"), None);
        assert_eq!(tree.count().expect("count() should succeed"), 1);

        cleanup(&path);
    }

    #[test]
    fn test_btree_multiple_inserts() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        tree.insert(b"c", b"3").expect("insert() should succeed");
        tree.insert(b"a", b"1").expect("insert() should succeed");
        tree.insert(b"b", b"2").expect("insert() should succeed");

        assert_eq!(
            tree.get(b"a").expect("get() should succeed"),
            Some(b"1".to_vec())
        );
        assert_eq!(
            tree.get(b"b").expect("get() should succeed"),
            Some(b"2".to_vec())
        );
        assert_eq!(
            tree.get(b"c").expect("get() should succeed"),
            Some(b"3".to_vec())
        );
        assert_eq!(tree.count().expect("count() should succeed"), 3);

        cleanup(&path);
    }

    #[test]
    fn test_btree_duplicate_key() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        tree.insert(b"key", b"value1")
            .expect("insert() should succeed");
        let result = tree.insert(b"key", b"value2");

        assert!(matches!(result, Err(BTreeError::DuplicateKey)));
        assert_eq!(
            tree.get(b"key").expect("get() should succeed"),
            Some(b"value1".to_vec())
        );

        cleanup(&path);
    }

    #[test]
    fn test_btree_delete() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        tree.insert(b"a", b"1").expect("insert() should succeed");
        tree.insert(b"b", b"2").expect("insert() should succeed");
        tree.insert(b"c", b"3").expect("insert() should succeed");

        assert!(tree.delete(b"b").expect("delete() should succeed"));
        assert!(!tree.delete(b"d").expect("delete() should succeed"));

        assert_eq!(
            tree.get(b"a").expect("get() should succeed"),
            Some(b"1".to_vec())
        );
        assert_eq!(tree.get(b"b").expect("get() should succeed"), None);
        assert_eq!(
            tree.get(b"c").expect("get() should succeed"),
            Some(b"3".to_vec())
        );
        assert_eq!(tree.count().expect("count() should succeed"), 2);

        cleanup(&path);
    }

    #[test]
    fn test_btree_delete_rebalance_removes_empty_leaf() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager.clone());

        let value = vec![b'v'; 200];
        for i in 0..300u32 {
            let key = format!("key{:03}", i);
            tree.insert(key.as_bytes(), &value)
                .expect("insert() should succeed");
        }

        // Preserve small end ranges while emptying all interior leaves.
        let keys: Vec<_> = (10..290u32)
            .map(|id| format!("key{id:03}").into_bytes())
            .collect();

        for key in &keys {
            tree.delete(key).expect("delete() should succeed");
        }

        let expected = 300 - keys.len();
        assert_eq!(tree.count().expect("count() should succeed"), expected);

        let mut cursor = tree.cursor_first().expect("cursor_first() should succeed");
        let mut results = Vec::new();
        while let Some((key, _)) = cursor.next().expect("next() should succeed") {
            results.push(key);
        }

        assert_eq!(results.len(), expected);
        let last_key = format!("key{:03}", 299).into_bytes();
        assert_eq!(results.last(), Some(&last_key));

        cleanup(&path);
    }

    #[test]
    fn test_btree_cursor() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        tree.insert(b"c", b"3").expect("insert() should succeed");
        tree.insert(b"a", b"1").expect("insert() should succeed");
        tree.insert(b"b", b"2").expect("insert() should succeed");

        let mut cursor = tree.cursor_first().expect("cursor_first() should succeed");
        let mut results: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

        while let Some(entry) = cursor.next().expect("next() should succeed") {
            results.push(entry);
        }

        assert_eq!(results.len(), 3);
        assert_eq!(results[0], (b"a".to_vec(), b"1".to_vec()));
        assert_eq!(results[1], (b"b".to_vec(), b"2".to_vec()));
        assert_eq!(results[2], (b"c".to_vec(), b"3".to_vec()));

        cleanup(&path);
    }

    #[test]
    fn test_btree_range() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        for i in 0..10u8 {
            let key = format!("key{:02}", i);
            let value = format!("val{:02}", i);
            tree.insert(key.as_bytes(), value.as_bytes())
                .expect("insert() should succeed");
        }

        let results = tree
            .range(b"key03", b"key06")
            .expect("range() should succeed");

        assert_eq!(results.len(), 4);
        assert_eq!(results[0].0, b"key03".to_vec());
        assert_eq!(results[3].0, b"key06".to_vec());

        cleanup(&path);
    }

    #[test]
    fn test_btree_large_keys() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        let key = vec![b'x'; 500];
        let value = vec![b'y'; 500];

        tree.insert(&key, &value).expect("insert() should succeed");
        assert_eq!(tree.get(&key).expect("get() should succeed"), Some(value));

        cleanup(&path);
    }

    #[test]
    fn test_btree_key_too_large() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        let key = vec![b'x'; MAX_KEY_SIZE + 1];
        let result = tree.insert(&key, b"value");

        assert!(matches!(result, Err(BTreeError::KeyTooLarge(_))));

        cleanup(&path);
    }

    #[test]
    fn test_btree_many_inserts() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        // Insert enough entries to force splits
        for i in 0..100u32 {
            let key = format!("key{:08}", i);
            let value = format!("value{:08}", i);
            tree.insert(key.as_bytes(), value.as_bytes())
                .expect("insert() should succeed");
        }

        // Verify all entries
        for i in 0..100u32 {
            let key = format!("key{:08}", i);
            let expected = format!("value{:08}", i);
            assert_eq!(
                tree.get(key.as_bytes()).expect("get() should succeed"),
                Some(expected.into_bytes())
            );
        }

        assert_eq!(tree.count().expect("count() should succeed"), 100);

        cleanup(&path);
    }

    #[test]
    fn test_btree_bulk_insert_sorted_preserves_leaf_layout() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        let items: Vec<(Vec<u8>, Vec<u8>)> = (0..240u32)
            .map(|i| {
                (
                    format!("key{:08}", i).into_bytes(),
                    vec![b'v'; 32 + (i as usize % 7)],
                )
            })
            .collect();

        tree.bulk_insert_sorted(&items)
            .expect("bulk_insert_sorted() should succeed");

        assert_eq!(tree.count().expect("count() should succeed"), items.len());

        for (key, value) in &items {
            assert_eq!(
                tree.get(key).expect("get() should succeed"),
                Some(value.clone())
            );
        }

        cleanup(&path);
    }

    /// Property test for `BTree::upsert_batch_sorted` (#159).
    ///
    /// Generates random batches of (key, value) updates and applies them to
    /// two independently-built trees: the baseline uses the existing
    /// loop-of-`upsert` path, and the candidate uses `upsert_batch_sorted`
    /// after the caller-side sort.  Both trees must end up with identical
    /// key→value contents (verified via per-key `get` and a full cursor
    /// scan), proving the new path is observationally equivalent for
    /// arbitrary batches up to 200 entries.
    mod prop_upsert_batch_sorted {
        use super::*;
        use proptest::prelude::*;
        use std::collections::BTreeMap;

        /// Build a tree pre-populated with `seed` and return the path so
        /// the caller can clean it up.
        fn populate_tree(seed: &[(Vec<u8>, Vec<u8>)]) -> (BTree, TempDbPath, Arc<Pager>) {
            let path = temp_db_path();
            cleanup(&path);
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(Arc::clone(&pager));
            for (k, v) in seed {
                tree.upsert(k, v).expect("upsert() should succeed");
            }
            (tree, path, pager)
        }

        fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
            // Short keys keep many entries per leaf so a single batch
            // hits multiple keys in the same leaf — the path the new
            // method optimises.  1..=4 bytes from a small alphabet.
            prop::collection::vec(0u8..16u8, 1..=4)
        }

        fn value_strategy() -> impl Strategy<Value = Vec<u8>> {
            // Tiny values — same-or-shrink fits the in-place overwrite
            // fast path; growing values fall back to the generic
            // single-key `upsert`, exercising both branches.
            prop::collection::vec(0u8..255u8, 0..=8)
        }

        fn pair_strategy() -> impl Strategy<Value = (Vec<u8>, Vec<u8>)> {
            (key_strategy(), value_strategy())
        }

        proptest! {
            #![proptest_config(ProptestConfig {
                cases: 64,
                .. ProptestConfig::default()
            })]

            #[test]
            fn batch_upsert_matches_loop_upsert(
                seed in prop::collection::vec(pair_strategy(), 0..50),
                batch in prop::collection::vec(pair_strategy(), 1..200),
            ) {
                // Dedup the seed by key (last write wins) so both trees
                // start from the same well-defined state.
                let mut seed_map: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
                for (k, v) in seed {
                    seed_map.insert(k, v);
                }
                let seed: Vec<(Vec<u8>, Vec<u8>)> = seed_map.into_iter().collect();

                // Baseline: loop-of-upsert.
                let (baseline, baseline_path, _baseline_pager) = populate_tree(&seed);
                for (k, v) in &batch {
                    baseline.upsert(k, v).expect("upsert() should succeed");
                }

                // Candidate: upsert_batch_sorted after caller-side sort
                // (matches the contract documented on the method).
                let (candidate, candidate_path, _candidate_pager) = populate_tree(&seed);
                let mut sorted = batch.clone();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                candidate.upsert_batch_sorted(&sorted).expect("upsert_batch_sorted() should succeed");

                // Compute the expected end state from BTreeMap (last
                // write wins), independent of insertion order.
                let mut expected: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
                for (k, v) in &seed {
                    expected.insert(k.clone(), v.clone());
                }
                for (k, v) in &batch {
                    expected.insert(k.clone(), v.clone());
                }

                // Per-key get matches expected on both trees.
                for (k, v) in &expected {
                    let baseline_got = baseline.get(k).expect("get() should succeed");
                    let candidate_got = candidate.get(k).expect("get() should succeed");
                    prop_assert_eq!(baseline_got.as_ref(), Some(v));
                    prop_assert_eq!(candidate_got.as_ref(), Some(v));
                }

                // Full cursor scan yields identical sorted contents.
                let collect = |t: &BTree| -> Vec<(Vec<u8>, Vec<u8>)> {
                    let mut out = Vec::new();
                    let mut cur = t.cursor_first().expect("cursor_first() should succeed");
                    while let Some(entry) = cur.next().expect("next() should succeed") {
                        out.push(entry);
                    }
                    out
                };
                let baseline_contents = collect(&baseline);
                let candidate_contents = collect(&candidate);
                prop_assert_eq!(&baseline_contents, &candidate_contents);

                // And both equal the BTreeMap-derived expectation.
                let expected_contents: Vec<(Vec<u8>, Vec<u8>)> =
                    expected.into_iter().collect();
                prop_assert_eq!(&candidate_contents, &expected_contents);

                // Drop the trees and pagers BEFORE cleanup. The pager flushes
                // its header (`.db-hdr`) on drop, so cleaning up while an
                // `Arc<Pager>` is still alive lets the final drop re-create the
                // sidecar AFTER removal — exactly the leak the guard
                // (scripts/check-temp-residue.sh) flags (128 `.db-hdr` files,
                // one per tree per proptest case). Explicit drops make cleanup
                // the last thing to touch these paths.
                drop(baseline);
                drop(candidate);
                drop(_baseline_pager);
                drop(_candidate_pager);
                cleanup(&baseline_path);
                cleanup(&candidate_path);
            }
        }
    }

    #[test]
    fn test_btree_sorted_iteration() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = BTree::new(pager);

        // Insert in random order
        let keys = vec![50, 25, 75, 10, 30, 60, 80, 5, 15, 27, 35, 55, 65, 77, 90];
        for k in &keys {
            let key = format!("{:03}", k);
            tree.insert(key.as_bytes(), key.as_bytes())
                .expect("insert() should succeed");
        }

        // Should iterate in sorted order
        let mut cursor = tree.cursor_first().expect("cursor_first() should succeed");
        let mut prev: Option<Vec<u8>> = None;

        while let Some((key, _)) = cursor.next().expect("next() should succeed") {
            if let Some(p) = &prev {
                assert!(p < &key, "Keys not in sorted order");
            }
            prev = Some(key);
        }

        cleanup(&path);
    }

    /// #2376: several threads writing one tree (the shape of concurrent
    /// DDL emitting `red.control_events` rows) must not lose keys or tear
    /// the tree through a racing copy-modify-write of the same leaf or a
    /// racing split.
    #[test]
    fn concurrent_writers_on_one_tree_keep_every_key() {
        const WRITER_COUNT: u32 = 4;
        const KEY_COUNT_PER_WRITER: u32 = 2_000;

        let path = temp_db_path();
        cleanup(&path);
        let pager = Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
        let tree = Arc::new(BTree::new(pager));
        let value = vec![7u8; 96];

        let writers: Vec<_> = (0..WRITER_COUNT)
            .map(|writer| {
                let tree = Arc::clone(&tree);
                let value = value.clone();
                std::thread::spawn(move || {
                    for i in 0..KEY_COUNT_PER_WRITER {
                        // Interleave the writers' key spaces so they share leaves.
                        let key = (i * WRITER_COUNT + writer).to_be_bytes();
                        tree.insert(&key, &value).expect("insert() should succeed");
                        // Churn: delete and re-insert every fourth key.
                        if i % 4 == 0 {
                            assert!(tree.delete(&key).expect("delete() should succeed"));
                            tree.upsert(&key, &value).expect("upsert() should succeed");
                        }
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().expect("writer thread panicked");
        }

        let key_count = WRITER_COUNT * KEY_COUNT_PER_WRITER;
        let mut cursor = tree.cursor_first().expect("cursor_first() should succeed");
        let mut seen = Vec::new();
        while let Some((key, _)) = cursor.next().expect("next() should succeed") {
            seen.push(u32::from_be_bytes(
                key.as_slice().try_into().expect("invariant: 4-byte keys"),
            ));
        }
        let expected: Vec<u32> = (0..key_count).collect();
        assert_eq!(seen, expected, "every key exactly once, in order");
        for key in 0..key_count {
            assert!(
                tree.get(&key.to_be_bytes())
                    .expect("get() should succeed")
                    .is_some(),
                "key {key} must be reachable from the root"
            );
        }

        cleanup(&path);
    }
}

/// Acceptance tests for slice E of PRD #662 — spill pipeline that wires
/// the per-cell value layout (`value_layout`), the LZ4 codec
/// (`vector_btree::value_codec`), and the overflow chain (`overflow`)
/// into the B-tree's `insert` / `bulk_insert_sorted` / `get` paths.
#[cfg(test)]
mod overflow_pipeline_tests {
    use super::value_layout::{MAX_VALUE_SIZE, OVERFLOW_THRESHOLD};
    use super::*;
    use std::path::PathBuf;

    fn temp_db_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "reddb_btree_spill_{}_{}_{}.db",
            tag,
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

    /// AC #1: a value at `OVERFLOW_THRESHOLD - 1` stores inline (raw or
    /// compressed) and **no overflow pages are allocated**. We count
    /// the file's pages before and after; the only growth should be the
    /// leaf page itself.
    #[test]
    fn inline_path_allocates_no_overflow_pages() {
        let path = temp_db_path("inline_no_overflow");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager.clone());
            let before = pager.page_count().expect("page_count() should succeed");

            let value = vec![0x5Au8; OVERFLOW_THRESHOLD - 1];
            tree.insert(b"k", &value).expect("insert() should succeed");

            let after = pager.page_count().expect("page_count() should succeed");
            // Exactly one new page (the root leaf). An overflow path
            // would have allocated ≥ 2 pages (leaf + chain head).
            assert_eq!(
                after - before,
                1,
                "inline value must allocate only the root leaf"
            );
            assert_eq!(tree.get(b"k").expect("get() should succeed"), Some(value));
        }
        cleanup(&path);
    }

    /// AC #2: a 44 KB highly-compressible markdown payload stores via
    /// the compressed-inline path (or via overflow if it still exceeds
    /// the threshold after LZ4) and `get` returns byte-identical bytes.
    #[test]
    fn compressible_44kb_round_trips() {
        let path = temp_db_path("compressible_44kb");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager);

            // Repeating markdown chunk — compresses far below threshold.
            let snippet = "# heading\n- bullet line that says the same thing over and over\n";
            let mut value = Vec::with_capacity(44 * 1024);
            while value.len() < 44 * 1024 {
                value.extend_from_slice(snippet.as_bytes());
            }
            assert!(value.len() > OVERFLOW_THRESHOLD);

            tree.insert(b"doc", &value)
                .expect("insert() should succeed");
            assert_eq!(tree.get(b"doc").expect("get() should succeed"), Some(value));
        }
        cleanup(&path);
    }

    /// AC #3: a 5 MB incompressible payload spills via overflow chain
    /// and `get` returns byte-identical bytes.
    #[test]
    fn incompressible_5mb_spills_and_round_trips() {
        let path = temp_db_path("incompressible_5mb");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager.clone());

            // xorshift produces incompressible bytes deterministically
            // so the test never flakes on the codec's heuristic.
            let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
            let value: Vec<u8> = (0..5 * 1024 * 1024)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect();

            let before = pager.page_count().expect("page_count() should succeed");
            tree.insert(b"blob", &value)
                .expect("insert() should succeed");
            let after = pager.page_count().expect("page_count() should succeed");
            // 5 MB / overflow-page payload ≈ many pages — the leaf
            // alone could not absorb this.
            assert!(
                after - before > 2,
                "5 MB spill must allocate overflow pages (alloc'd {} pages)",
                after - before
            );

            assert_eq!(
                tree.get(b"blob").expect("get() should succeed").as_deref(),
                Some(value.as_slice())
            );
        }
        cleanup(&path);
    }

    /// AC #4: a value just above `MAX_VALUE_SIZE` is rejected with
    /// `ValueTooLarge`; nothing is allocated (no LZ4 work, no overflow
    /// pages, no leaf page).
    #[test]
    fn value_above_ceiling_rejected_without_allocation() {
        let path = temp_db_path("ceiling_reject");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager.clone());
            let before = pager.page_count().expect("page_count() should succeed");

            let value = vec![0u8; MAX_VALUE_SIZE + 1];
            let err = tree.insert(b"too_big", &value).unwrap_err();
            assert!(matches!(err, BTreeError::ValueTooLarge(_)));

            assert_eq!(
                pager.page_count().expect("page_count() should succeed"),
                before,
                "rejected value must not allocate pages"
            );
            // Tree stays empty — no root leaf was created.
            assert!(tree.is_empty());
        }
        cleanup(&path);
    }

    /// AC #5: `bulk_insert_sorted` with mixed value sizes inserts all
    /// valid rows; an oversized row in the middle is rejected but the
    /// rest land. The function still surfaces the violation in its
    /// return value so callers do not silently lose data.
    #[test]
    fn bulk_insert_skips_oversized_row_keeps_rest() {
        let path = temp_db_path("bulk_mixed");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager);

            let mut items: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
            // Five small rows ...
            for i in 0..5u32 {
                items.push((format!("k{:04}", i).into_bytes(), vec![b'a' + i as u8; 64]));
            }
            // ... one oversize row in the middle ...
            items.push((b"k0005_huge".to_vec(), vec![0u8; MAX_VALUE_SIZE + 1]));
            // ... and five more small rows after it. Keys stay sorted.
            for i in 6..11u32 {
                items.push((format!("k{:04}", i).into_bytes(), vec![b'z' - i as u8; 32]));
            }

            let res = tree.bulk_insert_sorted(&items);
            assert!(matches!(res, Err(BTreeError::ValueTooLarge(_))));

            // All valid rows persisted.
            for (k, v) in items.iter().filter(|(_, v)| v.len() <= MAX_VALUE_SIZE) {
                assert_eq!(
                    tree.get(k).expect("get() should succeed").as_deref(),
                    Some(v.as_slice()),
                    "valid row {:?} must land",
                    k
                );
            }
            // The oversize row did not land.
            assert_eq!(tree.get(b"k0005_huge").expect("get() should succeed"), None);
        }
        cleanup(&path);
    }

    /// AC #6: values that used to fail (≥ 1024 bytes) now persist
    /// transparently. Walk a handful of sizes that straddle the
    /// threshold and the compressed-inline / spill boundaries.
    #[test]
    fn large_values_persist_transparently() {
        let path = temp_db_path("large_transparent");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager);

            // Sizes hit: just over threshold, well over threshold,
            // and several MB. Bytes pattern is the index — so the
            // values differ across keys and are incompressible enough
            // that LZ4 falls back to raw on the largest entries.
            for (i, size) in [
                OVERFLOW_THRESHOLD + 1,
                OVERFLOW_THRESHOLD * 2,
                64 * 1024,
                1 * 1024 * 1024,
            ]
            .iter()
            .enumerate()
            {
                let value: Vec<u8> = (0..*size).map(|n| ((n + i) & 0xff) as u8).collect();
                let key = format!("size{:08}", size).into_bytes();
                tree.insert(&key, &value).expect("insert() should succeed");
                assert_eq!(
                    tree.get(&key).expect("get() should succeed").as_deref(),
                    Some(value.as_slice()),
                    "size {} must round-trip",
                    size
                );
            }
        }
        cleanup(&path);
    }
}

/// Acceptance tests for slice F of PRD #662 — freeing overflow chains
/// on delete and shrinking update so storage does not leak.
#[cfg(test)]
mod overflow_free_tests {
    use super::value_layout::OVERFLOW_THRESHOLD;
    use super::*;
    use std::path::PathBuf;

    fn temp_db_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "reddb_btree_free_{}_{}_{}.db",
            tag,
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

    fn incompressible(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    /// AC #1: deleting a row whose value spilled returns every overflow
    /// page to the free list. Observable signal: page_count stays
    /// constant when we insert a same-size row right after the delete
    /// (no file growth on the second insert), and `tree.get` returns
    /// the new value.
    #[test]
    fn delete_spilled_value_frees_overflow_chain() {
        let path = temp_db_path("delete_frees");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager.clone());

            // First spilled insert grows the file.
            let v1 = incompressible(0xA11CE, 2 * 1024 * 1024);
            tree.insert(b"k", &v1).expect("insert() should succeed");
            let after_first = pager.page_count().expect("page_count() should succeed");

            // Delete: every overflow page returned to the free list.
            assert!(tree.delete(b"k").expect("delete() should succeed"));

            // Re-insert the same size. With a working free list, the
            // pager reuses the freed pages and the file does not grow.
            let v2 = incompressible(0xB0B, 2 * 1024 * 1024);
            tree.insert(b"k", &v2).expect("insert() should succeed");
            let after_second = pager.page_count().expect("page_count() should succeed");

            assert_eq!(
                after_second, after_first,
                "second spill must reuse freed pages, not grow the file"
            );
            assert_eq!(
                tree.get(b"k").expect("get() should succeed").as_deref(),
                Some(v2.as_slice())
            );
        }
        cleanup(&path);
    }

    /// AC #2: updating a spilled value to a small inline value frees
    /// the old chain in full. Same observability trick: insert another
    /// spilled value of the original size, see that the file does not
    /// grow beyond the original high-water mark.
    #[test]
    fn shrinking_update_to_inline_frees_chain() {
        let path = temp_db_path("shrink_to_inline");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager.clone());

            let big = incompressible(0xDEAD, 2 * 1024 * 1024);
            tree.insert(b"k", &big).expect("insert() should succeed");
            let hwm = pager.page_count().expect("page_count() should succeed");

            // Shrink to inline raw (fits well under the threshold).
            let small = b"tiny".to_vec();
            tree.upsert(b"k", &small).expect("upsert() should succeed");
            assert_eq!(
                tree.get(b"k").expect("get() should succeed").as_deref(),
                Some(small.as_slice())
            );

            // If the chain was not freed, this insert would push the
            // page count past `hwm`. If it was, the pager reuses freed
            // pages and we stay at the high-water mark.
            let big2 = incompressible(0xBEEF, 2 * 1024 * 1024);
            tree.insert(b"k2", &big2).expect("insert() should succeed");
            assert_eq!(
                pager.page_count().expect("page_count() should succeed"),
                hwm,
                "shrinking update must free the chain — replacement spill should reuse"
            );
            assert_eq!(
                tree.get(b"k2").expect("get() should succeed").as_deref(),
                Some(big2.as_slice())
            );
        }
        cleanup(&path);
    }

    /// AC #3: updating a spilled value to a shorter (still spilled)
    /// value frees the old chain in full. The new chain is independent
    /// — no in-place reuse — so the freed pages must show up in the
    /// allocator before any new growth happens.
    #[test]
    fn shrinking_update_to_shorter_spill_frees_old_chain() {
        let path = temp_db_path("shrink_spill_to_spill");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager.clone());

            // Two spilled values, the second clearly shorter than the
            // first. Both encode through the pointer path because the
            // bytes are incompressible.
            let big = incompressible(0xF00D, 3 * 1024 * 1024);
            tree.insert(b"k", &big).expect("insert() should succeed");
            let hwm_after_big = pager.page_count().expect("page_count() should succeed");

            // Per slice F's acceptance the new chain is *independent*
            // of the old — i.e. no in-place page reuse — so the
            // upsert allocates fresh pages for the smaller chain
            // before freeing the old one. The file may grow during
            // this window; what we care about is that the freed pages
            // re-enter the allocator. We measure that by dropping the
            // row entirely (which frees the new chain too) and
            // re-inserting at the original size: if both chains were
            // freed, the pager reuses freed pages and we do not exceed
            // the high-water mark observed during the transient.
            let smaller = incompressible(0xCAFE, 1 * 1024 * 1024);
            tree.upsert(b"k", &smaller)
                .expect("upsert() should succeed");
            assert_eq!(
                tree.get(b"k").expect("get() should succeed").as_deref(),
                Some(smaller.as_slice())
            );
            let hwm_after_upsert = pager.page_count().expect("page_count() should succeed");

            assert!(tree.delete(b"k").expect("delete() should succeed"));

            let again = incompressible(0x1234, 3 * 1024 * 1024);
            tree.insert(b"k", &again).expect("insert() should succeed");
            assert!(
                pager.page_count().expect("page_count() should succeed") <= hwm_after_upsert,
                "after delete+insert of original size, file must not grow past the upsert transient \
                 (page_count = {}, transient high-water = {}, original = {}) — the upsert must have \
                 freed the old chain so the re-insert reuses freed pages",
                pager.page_count().expect("page_count() should succeed"),
                hwm_after_upsert,
                hwm_after_big,
            );
            assert_eq!(
                tree.get(b"k").expect("get() should succeed").as_deref(),
                Some(again.as_slice())
            );
        }
        cleanup(&path);
    }

    /// AC #5 regression: insert large → delete → insert same-size large
    /// → file does not grow on the second insert.
    #[test]
    fn insert_delete_reinsert_does_not_grow_file() {
        let path = temp_db_path("reinsert_no_grow");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager.clone());

            let v1 = incompressible(0x5EED, 4 * 1024 * 1024);
            tree.insert(b"row", &v1).expect("insert() should succeed");
            let pages_after_first = pager.page_count().expect("page_count() should succeed");

            assert!(tree.delete(b"row").expect("delete() should succeed"));

            let v2 = incompressible(0xACE, 4 * 1024 * 1024);
            tree.insert(b"row", &v2).expect("insert() should succeed");
            let pages_after_second = pager.page_count().expect("page_count() should succeed");

            assert_eq!(
                pages_after_second, pages_after_first,
                "re-inserting at the same size must reuse the freed chain"
            );
            assert_eq!(
                tree.get(b"row").expect("get() should succeed").as_deref(),
                Some(v2.as_slice())
            );
        }
        cleanup(&path);
    }

    /// Belt-and-braces: an in-place shrink via `upsert_batch_sorted`
    /// — same in-place fast path as single-key `upsert` — must also
    /// free the old chain. Without this hook the batch path would
    /// silently leak on every `bulk_update` over spilled rows.
    #[test]
    fn upsert_batch_in_place_shrink_frees_chain() {
        let path = temp_db_path("batch_shrink");
        cleanup(&path);
        {
            let pager =
                Arc::new(Pager::open_default(&path).expect("open_default() should succeed"));
            let tree = BTree::new(pager.clone());

            // Seed two spilled rows so the batch update lands in-place
            // on the same leaf.
            let big_a = incompressible(0x1111, 2 * 1024 * 1024);
            let big_b = incompressible(0x2222, 2 * 1024 * 1024);
            tree.insert(b"a", &big_a).expect("insert() should succeed");
            tree.insert(b"b", &big_b).expect("insert() should succeed");
            let hwm = pager.page_count().expect("page_count() should succeed");

            let updates = vec![
                (b"a".to_vec(), b"shrunk-a".to_vec()),
                (b"b".to_vec(), b"shrunk-b".to_vec()),
            ];
            tree.upsert_batch_sorted(&updates)
                .expect("upsert_batch_sorted() should succeed");
            assert_eq!(
                tree.get(b"a").expect("get() should succeed").as_deref(),
                Some(b"shrunk-a".as_slice())
            );
            assert_eq!(
                tree.get(b"b").expect("get() should succeed").as_deref(),
                Some(b"shrunk-b".as_slice())
            );

            // Both chains must be freed: a fresh pair of spilled
            // inserts re-uses them instead of growing the file.
            let again_a = incompressible(0xAAAA, 2 * 1024 * 1024);
            let again_b = incompressible(0xBBBB, 2 * 1024 * 1024);
            tree.insert(b"c", &again_a)
                .expect("insert() should succeed");
            tree.insert(b"d", &again_b)
                .expect("insert() should succeed");
            assert!(
                pager.page_count().expect("page_count() should succeed") <= hwm,
                "batch in-place shrink must free chains — replacement spills should reuse"
            );
        }
        cleanup(&path);
    }
}
