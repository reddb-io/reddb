use super::*;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TEST: &str = "runtime::page_integrity_faults::catalog_publication_faults::catalog_generation_publication_survives_crash_and_replays_confirmed_rows";
const CHILD: &str = "REDDB_CATALOG_PUBLICATION_PATH";

fn reference(path: &Path) -> reddb_file::page_proof::PageReference {
    let bytes = std::fs::read(path).expect("inspect persisted superblocks");
    let selected = reddb_file::select_paged_superblock(&bytes).expect("valid superblock");
    let start = selected.copy_index * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
    reddb_file::decode_database_header(
        &bytes[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE],
    )
    .expect("decode catalog authority")
    .manifest_reference
}

fn run_child(path: &Path, phase: &str, expected: i32) {
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, path)
        .env("REDDB_CATALOG_PUBLICATION_PHASE", phase)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("publication child");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("child status").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("stop stalled publication");
            let output = child.wait_with_output().expect("stalled child output");
            panic!(
                "catalog publication stalled: {phase}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("publication exit");
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{phase}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Homes are already durable at the before-publication crash. Publish only
/// one sealed header to reproduce a crash between the two superblock writes.
pub(super) fn publish_one_slot(path: &Path) -> reddb_file::page_proof::PageReference {
    let bytes = std::fs::read(path).expect("prepared catalog pages");
    let previous = reference(path);
    let prior_offset = usize::try_from(if previous.page_id == 0 {
        1
    } else {
        previous.page_id
    })
    .expect("prior root")
        * PAGE_SIZE;
    let prior =
        Page::from_slice(&bytes[prior_offset..prior_offset + PAGE_SIZE]).expect("prior catalog");
    let epoch =
        reddb_file::native_store::decode_native_paged_catalog_ownership_epoch(prior.content())
            .expect("prior catalog epoch")
            .expect("checkpoint epoch");
    let next = bytes
        .chunks_exact(PAGE_SIZE)
        .enumerate()
        .find_map(|(id, bytes)| {
            if id < reddb_engine::storage::pager_io::FIRST_ALLOCATABLE_PAGE as usize {
                return None;
            }
            let page = Page::from_slice(bytes).ok()?;
            if page.page_type().ok()? != PageType::Header || page.verify_checksum().is_err() {
                return None;
            }
            let candidate = reddb_file::native_store::decode_native_paged_catalog_ownership_epoch(
                page.content(),
            )
            .ok()??;
            if candidate <= epoch {
                return None;
            }
            assert!(
                reddb_file::decode_native_metadata_overflow_header(page.content())
                    .expect("framing")
                    .is_none(),
                "the partial-publication fixture must have one catalog page"
            );
            Some(reddb_file::page_proof::PageReference {
                page_id: page.page_id(),
                digest: reddb_file::page_proof::page_digest(page.as_bytes()),
            })
        })
        .expect("unpublished new catalog generation");
    let selected = reddb_file::select_paged_superblock(&bytes).expect("selected header");
    let start = selected.copy_index * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
    let mut slot = [0; reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE];
    slot.copy_from_slice(&bytes[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE]);
    let mut header = reddb_file::decode_database_header(&slot).expect("current header");
    header.manifest_reference = next;
    reddb_file::encode_database_header(&mut slot, &header).expect("new catalog authority");
    let stale_copy = 1 - selected.copy_index;
    reddb_file::seal_paged_superblock_slot(
        &mut slot,
        stale_copy,
        selected.generation.checked_add(1).expect("next generation"),
    )
    .expect("seal one publication copy");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("fault target");
    file.seek(SeekFrom::Start(
        u64::try_from(stale_copy * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE).expect("slot offset"),
    ))
    .expect("publication slot");
    file.write_all(&slot).expect("publish first slot only");
    file.sync_all().expect("durable partial publication");
    next
}

fn assert_rows(runtime: &RedDBRuntime, progress: bool) {
    let point = runtime
        .execute_query("SELECT payload FROM records WHERE id = 1")
        .expect("cold HASH after catalog recovery");
    assert_eq!(point.result.records.len(), 1);
    assert_eq!(
        point.result.records[0].get("payload"),
        Some(&Value::text("committed"))
    );
    let indexed = runtime
        .execute_query(
            "SELECT id,payload FROM records WHERE payload >= 'a' AND payload < 'z' ORDER BY id",
        )
        .expect("cold BTREE after catalog recovery");
    let scan = runtime
        .execute_query("SELECT id,payload FROM records ORDER BY id")
        .expect("authoritative scan after catalog recovery");
    for result in [&indexed, &scan] {
        assert_eq!(result.result.records.len(), if progress { 3 } else { 2 });
        for (record, (id, payload)) in
            result
                .result
                .records
                .iter()
                .zip([(1, "committed"), (2, "keep"), (3, "progress")])
        {
            assert_eq!(record.get("id"), Some(&Value::Integer(id)));
            assert_eq!(record.get("payload"), Some(&Value::text(payload)));
        }
    }
    assert!(runtime
        .verify_persisted_indexes("records")
        .expect("persisted index integrity")
        .is_empty());
}

#[test]
fn catalog_generation_publication_survives_crash_and_replays_confirmed_rows() {
    if let Some(path) = std::env::var_os(CHILD) {
        let phase = std::env::var("REDDB_CATALOG_PUBLICATION_PHASE").expect("crash phase");
        if phase == "recover-release" {
            std::env::set_var("REDDB_EXPERIMENTAL_EXIT_DURING_OWNERSHIP_RECOVERY", "1");
            let _ = open(Path::new(&path)).expect("crash after recovery release");
            panic!("ownership recovery crash was not reached");
        }
        let runtime = open(Path::new(&path)).expect("healthy writer before publication crash");
        runtime
            .execute_query("UPDATE records SET payload = 'committed' WHERE id = 1")
            .expect("confirmed MVCC update");
        std::env::set_var(
            if phase == "before" {
                "REDDB_TEST_EXIT_BEFORE_CATALOG_ROOT_PUBLICATION"
            } else {
                "REDDB_TEST_EXIT_AFTER_CATALOG_ROOT_PUBLICATION"
            },
            "1",
        );
        runtime
            .graceful_shutdown(false)
            .expect("injected process exit");
        panic!("catalog publication crash was not reached");
    }
    for phase in ["before", "after", "one-slot"] {
        let directory = tempfile::tempdir().expect("isolated publication fault");
        let path = directory.path().join("catalog-publication.rdb");
        seed(&path);
        let previous = reference(&path);
        run_child(
            &path,
            if phase == "after" { "after" } else { "before" },
            if phase == "after" { 198 } else { 197 },
        );
        if phase == "one-slot" {
            let selected = publish_one_slot(&path);
            run_child(&path, "recover-release", 187);
            let mut bytes = std::fs::read(&path).expect("interrupted allocator recovery");
            for copy in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
                let start = copy * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
                let slot = &bytes[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE];
                assert!(reddb_file::paged_superblock_slot_generation(slot, copy).is_some());
                assert_eq!(
                    reddb_file::decode_database_header(slot)
                        .expect("fenced recovery slot")
                        .manifest_reference,
                    selected,
                    "old catalog must be unreachable before allocator release"
                );
            }
            let newest =
                reddb_file::select_paged_superblock(&bytes).expect("newest recovered slot");
            bytes[newest.copy_index * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE] ^= 1;
            std::fs::write(&path, bytes).expect("damage newest slot after another crash");
        }
        if phase == "before" {
            assert_eq!(reference(&path), previous);
        } else {
            assert_ne!(reference(&path), previous);
        }
        for round in 0..2 {
            let started = Instant::now();
            let runtime = open(&path).expect("reopen with complete catalog authority");
            assert_rows(&runtime, round != 0);
            if round == 0 {
                runtime
                    .execute_query("INSERT INTO records (id,payload) VALUES (1,'duplicate')")
                    .expect_err("UNIQUE must survive publication recovery");
                runtime
                    .execute_query("INSERT INTO records (id,payload) VALUES (3,'progress')")
                    .expect("writer progresses after recovery");
            }
            runtime
                .graceful_shutdown(false)
                .expect("checkpoint recovered generation");
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "recovery must make bounded progress"
            );
        }
    }
}
