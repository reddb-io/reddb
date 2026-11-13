use reddb_engine::storage::{
    btree::BTree,
    page::PageType,
    pager::{NoWal, Pager, PagerConfig},
    pager_io::FIRST_ALLOCATABLE_PAGE,
};
use reddb_file::{OpenMode, StdVfs, Vfs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const TEST: &str = "checksum_valid_overflow_cycle_terminates_with_corruption_error";
const CHILD_PATH: &str = "REDDB_OVERFLOW_CYCLE_PATH";
const CHILD_ROOT: &str = "REDDB_OVERFLOW_CYCLE_ROOT";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "reddb-overflow-cycle-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create fixture directory");
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open(path: &Path, create: bool) -> Arc<Pager<<StdVfs as Vfs>::File, NoWal>> {
    let file = StdVfs
        .open(path, OpenMode::create_keep())
        .expect("open page file");
    Arc::new(
        Pager::open_owned(
            file,
            path.to_path_buf(),
            PagerConfig::default(),
            NoWal,
            create,
        )
        .expect("open pager"),
    )
}

fn noise(bytes: usize) -> Vec<u8> {
    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    (0..bytes)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn child(path: &Path, root: u32) {
    let pager = open(path, false);
    let tree = BTree::with_root(pager, root);
    let error = tree
        .get(b"durable-row")
        .expect_err("cyclic overflow chain must be rejected");
    assert!(
        error.to_string().contains("cyclic overflow chain"),
        "explicit corruption error: {error}"
    );
    std::process::exit(217);
}

fn run_child(path: &Path, root: u32) {
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD_PATH, path)
        .env(CHILD_ROOT, root.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("overflow recovery child");
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().expect("child status").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("stop nonterminating recovery");
            let output = child.wait_with_output().expect("stuck recovery output");
            panic!(
                "checksum-valid overflow cycle made no progress: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("overflow child output");
    assert_eq!(
        output.status.code(),
        Some(217),
        "overflow recovery must report corruption: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn checksum_valid_overflow_cycle_terminates_with_corruption_error() {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let root = std::env::var(CHILD_ROOT)
            .expect("root")
            .parse()
            .expect("numeric root");
        child(Path::new(&path), root);
    }

    let fixture = Fixture::new();
    let path = fixture.0.join("cycle.rdb");
    let pager = open(&path, true);
    let tree = BTree::new(Arc::clone(&pager));
    let value = noise(48 * 1024);
    tree.insert(b"durable-row", &value).expect("overflow row");
    pager.sync().expect("persist row");
    let root = tree.root_page_id();
    drop(tree);
    drop(pager);

    let pager = open(&path, false);
    let recovered = BTree::with_root(Arc::clone(&pager), root);
    assert_eq!(
        recovered.get(b"durable-row").expect("cold healthy read"),
        Some(value.clone())
    );
    drop(recovered);

    let (page_id, mut page) = (FIRST_ALLOCATABLE_PAGE..pager.page_count().unwrap())
        .filter_map(|page_id| pager.read_page(page_id).ok().map(|page| (page_id, page)))
        .find(|(_, page)| {
            page.page_type().ok() == Some(PageType::Overflow)
                && page
                    .content()
                    .windows(64)
                    .any(|bytes| bytes == &value[..64])
        })
        .expect("large value overflow page");
    page.content_mut()[0..4].copy_from_slice(&page_id.to_le_bytes());
    page.content_mut()[4..8].copy_from_slice(&0_u32.to_le_bytes());
    pager.write_page(page_id, page).expect("inject valid cycle");
    pager.sync().expect("persist valid cycle");
    drop(pager);

    run_child(&path, root);
}
