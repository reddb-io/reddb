use reddb_server::storage::engine::{BTree, Pager, PagerConfig};
use std::sync::Arc;

#[test]
fn mixed_width_leaf_split_preserves_all_values() {
    let directory = tempfile::tempdir().unwrap();
    for bulk in [false, true] {
        let path = directory.path().join(format!("mixed-{bulk}.rdb"));
        let pager = Arc::new(
            Pager::open(
                &path,
                PagerConfig {
                    cache_size: 4,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        let tree = BTree::new(Arc::clone(&pager));
        let records: Vec<_> = (0..47u64)
            .map(|id| {
                (
                    id.to_be_bytes().to_vec(),
                    vec![id as u8; if id < 31 { 1 } else { 1024 }],
                )
            })
            .collect();
        if bulk {
            tree.bulk_insert_sorted(&records)
                .expect("mixed-width bulk split");
        } else {
            for (key, value) in &records {
                tree.insert(key, value).expect("mixed-width single split");
            }
        }
        pager.sync().unwrap();
        let root = tree.root_page_id();
        drop(tree);
        drop(pager);
        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::with_root(pager, root);
        let mut cursor = tree.cursor_first().unwrap();
        for record in &records {
            assert_eq!(cursor.next().unwrap().as_ref(), Some(record));
        }
        assert!(cursor.next().unwrap().is_none());
    }
}
