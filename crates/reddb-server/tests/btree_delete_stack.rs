//! Cascading B-tree merges must fit the maintenance worker's ordinary stack.
use std::process::Command;
use std::sync::Arc;

use reddb_server::storage::engine::{BTree, PageType, Pager};

fn fixture_key(id: u32) -> Vec<u8> {
    let mut key = vec![b'x'; 1024];
    key[..4].copy_from_slice(&id.to_be_bytes());
    key
}

#[test]
fn cascading_deletes_fit_a_two_mib_worker_stack() {
    const CHILD: &str = "REDDB_BTREE_STACK_TEST_CHILD";
    const DIRECTORY: &str = "REDDB_BTREE_STACK_TEST_DIRECTORY";
    if let Ok(order) = std::env::var(CHILD) {
        let directory =
            std::path::PathBuf::from(std::env::var_os(DIRECTORY).expect("child fixture path"));
        assert_eq!(
            std::fs::read(directory.join("owned-stack-fixture")).expect("fixture owner marker"),
            b"btree-stack-v1"
        );
        let path = directory.join("tree.rdb");
        let (pager, tree) = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let pager = Arc::new(Pager::open_default(&path).expect("fixture pager opens"));
                let tree = BTree::new(pager.clone());
                let rows = (0..32768)
                    .map(|id| (fixture_key(id), id.to_be_bytes().to_vec()))
                    .collect::<Vec<_>>();
                tree.bulk_insert_sorted(&rows).expect("seed a deep tree");
                let mut page = tree.root_page_id();
                let mut levels = 0;
                for _ in 0..16 {
                    let current = pager.read_page(page).expect("read fixture level");
                    levels += 1;
                    if current.page_type().expect("valid page type") == PageType::BTreeLeaf {
                        break;
                    }
                    page = current.right_child();
                }
                assert!(
                    levels >= 5,
                    "fixture must exercise cascading interior merges"
                );
                (pager, tree)
            })
            .expect("seed worker starts")
            .join()
            .expect("seed worker completes");
        std::thread::Builder::new()
            .name("small-stack-delete".to_string())
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let ids: Vec<u32> = if order == "ascending" {
                    (0..32767).collect()
                } else {
                    (1..32768).rev().collect()
                };
                for id in ids {
                    assert!(tree
                        .delete(&fixture_key(id))
                        .expect("cascading delete succeeds"));
                }
                let remaining = if order == "ascending" { 32767u32 } else { 0u32 };
                assert_eq!(tree.count().expect("count remaining keys"), 1);
                assert_eq!(
                    tree.get(&fixture_key(remaining))
                        .expect("read remaining key"),
                    Some(remaining.to_be_bytes().to_vec())
                );
                assert!(tree
                    .delete(&fixture_key(remaining))
                    .expect("remove final root"));
                assert!(tree.is_empty());
                drop(tree);
                drop(pager);
            })
            .expect("small-stack worker starts")
            .join()
            .expect("small-stack worker completes");
        return;
    }
    // A regression aborts only its child, so the parent can report the failure
    // and remove its own temporary fixture instead of leaving test databases.
    for order in ["ascending", "descending"] {
        let directory = tempfile::tempdir().expect("isolated fixture directory");
        std::fs::write(
            directory.path().join("owned-stack-fixture"),
            b"btree-stack-v1",
        )
        .expect("mark fixture owner");
        let result = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "cascading_deletes_fit_a_two_mib_worker_stack",
                "--nocapture",
            ])
            .env(CHILD, order)
            .env(DIRECTORY, directory.path())
            .output()
            .expect("run isolated stack regression");
        drop(directory);
        assert!(
            result.status.success(),
            "{order} deletion failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
