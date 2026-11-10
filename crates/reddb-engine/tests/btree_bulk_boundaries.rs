use reddb_engine::storage::{
    btree::{BTree, BTreeError},
    page::{Page, PageType, HEADER_SIZE},
    pager::{NoWal, Pager, PagerConfig},
    pager_io,
};
use reddb_file::{OpenMode, StdVfs, Vfs};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::SystemTime};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "reddb-bulk-boundary-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create isolated fixture directory");
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn leaf_key(page: &Page, index: usize) -> Vec<u8> {
    let bytes = page.as_bytes();
    let slot = HEADER_SIZE + 8 + index * 2;
    let offset = u16::from_le_bytes([bytes[slot], bytes[slot + 1]]) as usize;
    let len = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
    bytes[offset + 4..offset + 4 + len].to_vec()
}

fn next_leaf(page: &Page) -> u32 {
    let bytes = page.as_bytes();
    u32::from_le_bytes(
        bytes[HEADER_SIZE + 4..HEADER_SIZE + 8]
            .try_into()
            .expect("link"),
    )
}

fn interior_key_child(page: &Page, index: usize) -> (Vec<u8>, u32) {
    let bytes = page.as_bytes();
    let slot = HEADER_SIZE + index * 2;
    let offset = u16::from_le_bytes([bytes[slot], bytes[slot + 1]]) as usize;
    let len = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
    let key = bytes[offset + 2..offset + 2 + len].to_vec();
    let child = u32::from_le_bytes(
        bytes[offset + 2 + len..offset + 6 + len]
            .try_into()
            .expect("child"),
    );
    (key, child)
}

#[test]
fn batch_respects_ancestor_separator_after_first_key_is_deleted() {
    let fixture = Fixture::new();
    let path = fixture.0.join("ancestor.rdb");
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("open file");
    let pager = Arc::new(
        Pager::open_owned(file, path.clone(), PagerConfig::default(), NoWal, true)
            .expect("create pager"),
    );
    let tree = BTree::new(pager.clone());
    let mut expected = BTreeMap::new();
    for id in (0..3200u32).step_by(2) {
        let key = format!("{id:08}{}", "x".repeat(512)).into_bytes();
        tree.insert(&key, &[42; 300]).expect("seed");
        expected.insert(key, vec![42; 300]);
    }
    let root = pager.read_page(tree.root_page_id()).expect("root");
    assert_eq!(
        root.page_type().expect("root type"),
        PageType::BTreeInterior
    );
    let (boundary, first_child) = interior_key_child(&root, 0);
    assert_eq!(
        pager
            .read_page(first_child)
            .expect("child")
            .page_type()
            .expect("child type"),
        PageType::BTreeInterior,
        "fixture must have multiple interior levels"
    );
    let mut left_id = pager_io::FIRST_ALLOCATABLE_PAGE;
    let right_id = loop {
        let page = pager.read_page(left_id).expect("left leaf");
        let next = next_leaf(&page);
        assert_ne!(next, 0, "ancestor separator must appear in the leaf chain");
        let right = pager.read_page(next).expect("right leaf");
        if leaf_key(&right, 0) == boundary {
            break next;
        }
        left_id = next;
    };
    assert!(tree.delete(&boundary).expect("delete boundary key"));
    expected.remove(&boundary);
    let root_after = pager
        .read_page(tree.root_page_id())
        .expect("root after delete");
    assert_eq!(
        interior_key_child(&root_after, 0).0,
        boundary,
        "ancestor retains the lower separator"
    );
    let right = pager.read_page(right_id).expect("right after delete");
    assert!(
        leaf_key(&right, 0) > boundary,
        "deleted key leaves a gap in the right subtree"
    );
    let left = pager.read_page(left_id).expect("left after delete");
    let mut gap = leaf_key(&left, usize::from(left.cell_count()) - 1);
    gap.push(b'~');
    assert!(gap < boundary);
    tree.bulk_insert_sorted(&[
        (gap.clone(), b"left".to_vec()),
        (boundary.clone(), b"restored".to_vec()),
    ])
    .expect("insert sorted keys spanning the ancestor separator");
    expected.insert(gap.clone(), b"left".to_vec());
    expected.insert(boundary.clone(), b"restored".to_vec());
    assert_eq!(tree.get(&gap).expect("left lookup"), Some(b"left".to_vec()));
    assert_eq!(
        tree.get(&boundary).expect("right lookup"),
        Some(b"restored".to_vec())
    );
    let mut leaf_id = pager_io::FIRST_ALLOCATABLE_PAGE;
    let mut additions = Vec::new();
    loop {
        let leaf = pager.read_page(leaf_id).expect("linked leaf");
        let mut key = leaf_key(&leaf, usize::from(leaf.cell_count()) - 1);
        key.push(b'~');
        let next = next_leaf(&leaf);
        if next != 0 {
            assert!(key < leaf_key(&pager.read_page(next).expect("next leaf"), 0));
        }
        additions.push((key.clone(), b"gap".to_vec()));
        expected.insert(key, b"gap".to_vec());
        if next == 0 {
            break;
        }
        leaf_id = next;
    }
    assert!(additions.len() >= 3);
    tree.bulk_insert_sorted(&additions)
        .expect("batch across multilevel tree");
    pager.sync().expect("persist multilevel tree");
    let root_id = tree.root_page_id();
    drop(tree);
    drop(pager);
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("reopen file");
    let pager = Arc::new(
        Pager::open_owned(file, path, PagerConfig::default(), NoWal, false).expect("reopen pager"),
    );
    let recovered = BTree::with_root(pager, root_id);
    let mut cursor = recovered.cursor_first().expect("recovered cursor");
    for entry in &expected {
        assert_eq!(
            cursor
                .next()
                .expect("ordered scan")
                .as_ref()
                .map(|(key, value)| (key, value)),
            Some(entry)
        );
        assert_eq!(
            recovered.get(entry.0).expect("lookup").as_ref(),
            Some(entry.1)
        );
    }
    assert!(cursor.next().expect("end").is_none());
}

#[test]
fn sorted_batch_crossing_populated_leaves_preserves_every_record_after_reopen() {
    let fixture = Fixture::new();
    let path = fixture.0.join("data.rdb");
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("open file");
    let pager = Arc::new(
        Pager::open_owned(file, path.clone(), PagerConfig::default(), NoWal, true)
            .expect("create pager"),
    );
    let tree = BTree::new(pager.clone());
    let mut expected = BTreeMap::new();
    for id in 0..250u32 {
        let key = format!("z{id:04}").into_bytes();
        tree.insert(&key, &[42; 300]).expect("seed");
        expected.insert(key, vec![42; 300]);
    }
    let mut leaf_id = pager_io::FIRST_ALLOCATABLE_PAGE;
    let mut additions = Vec::new();
    loop {
        let leaf = pager.read_page(leaf_id).expect("read leaf");
        let mut gap = leaf_key(&leaf, usize::from(leaf.cell_count()) - 1);
        gap.push(b'~');
        let next = next_leaf(&leaf);
        if next != 0 {
            assert!(gap < leaf_key(&pager.read_page(next).expect("read sibling"), 0));
        }
        additions.push((gap.clone(), b"gap".to_vec()));
        expected.insert(gap, b"gap".to_vec());
        if next == 0 {
            break;
        }
        leaf_id = next;
    }
    assert!(additions.len() >= 3, "fixture must span multiple leaves");
    tree.bulk_insert_sorted(&additions)
        .expect("insert ordered batch");
    let mut cursor = tree.cursor_first().expect("cursor");
    for entry in &expected {
        assert_eq!(
            cursor
                .next()
                .expect("ordered scan")
                .as_ref()
                .map(|(key, value)| (key, value)),
            Some(entry)
        );
    }
    assert!(cursor.next().expect("end").is_none());
    drop(cursor);
    pager.sync().expect("persist fixture");
    let root = tree.root_page_id();
    drop(tree);
    drop(pager);
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("reopen file");
    let reopened = Arc::new(
        Pager::open_owned(file, path, PagerConfig::default(), NoWal, false).expect("reopen pager"),
    );
    let recovered = BTree::with_root(reopened, root);
    let mut cursor = recovered.cursor_first().expect("recovered cursor");
    for entry in &expected {
        assert_eq!(
            cursor
                .next()
                .expect("recovered scan")
                .as_ref()
                .map(|(key, value)| (key, value)),
            Some(entry)
        );
        assert_eq!(
            recovered.get(entry.0).expect("point lookup").as_ref(),
            Some(entry.1)
        );
    }
    assert!(cursor.next().expect("end").is_none());
}

#[test]
fn duplicate_at_next_leaf_boundary_is_rejected_without_corrupting_existing_rows() {
    let fixture = Fixture::new();
    let path = fixture.0.join("duplicates.rdb");
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("open file");
    let pager = Arc::new(
        Pager::open_owned(file, path.clone(), PagerConfig::default(), NoWal, true)
            .expect("create pager"),
    );
    let tree = BTree::new(pager.clone());
    tree.insert(b"z0000", &[42; 300]).expect("seed");
    let first_leaf = tree.root_page_id();
    let mut n = 0u32;
    while tree.root_page_id() == first_leaf {
        n += 1;
        tree.insert(format!("z{n:04}").as_bytes(), &[42; 300])
            .expect("split fixture");
        assert!(n < 200);
    }
    let left = pager.read_page(first_leaf).expect("left leaf");
    let next = next_leaf(&left);
    assert_ne!(next, 0);
    let right = pager.read_page(next).expect("right leaf");
    let mut gap = leaf_key(&left, usize::from(left.cell_count()) - 1);
    gap.push(b'~');
    let duplicate = leaf_key(&right, 0);
    assert!(gap < duplicate);
    let error = tree
        .bulk_insert_sorted(&[
            (gap, b"gap".to_vec()),
            (duplicate.clone(), b"wrong".to_vec()),
        ])
        .expect_err("existing key must be rejected");
    assert!(
        matches!(error, BTreeError::DuplicateKey),
        "unexpected error: {error}"
    );
    assert_eq!(
        tree.get(&duplicate).expect("original lookup"),
        Some(vec![42; 300])
    );
    pager.sync().expect("persist after rejected batch");
    let root = tree.root_page_id();
    drop(tree);
    drop(pager);
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("reopen file");
    let pager = Arc::new(
        Pager::open_owned(file, path, PagerConfig::default(), NoWal, false).expect("reopen pager"),
    );
    let recovered = BTree::with_root(pager, root);
    for id in 0..=n {
        assert_eq!(
            recovered
                .get(format!("z{id:04}").as_bytes())
                .expect("lookup"),
            Some(vec![42; 300])
        );
    }
    let mut cursor = recovered.cursor_first().expect("cursor");
    let mut previous: Option<Vec<u8>> = None;
    while let Some((key, _)) = cursor.next().expect("ordered scan") {
        assert!(previous.as_ref().is_none_or(|prior| prior < &key));
        previous = Some(key);
    }
}
