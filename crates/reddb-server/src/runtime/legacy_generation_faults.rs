use super::*;
use crate::storage::engine::BTree;
use std::collections::HashSet;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "REDDB_LEGACY_GENERATION_PATH";
const TEST: &str = "runtime::page_integrity_faults::legacy_generation_faults::legacy_native_migration_survives_crash_and_reclaims_verified_generations";

fn payload() -> String {
    let mut state = 0x4d595df4d0f33173_u64;
    (0..48_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(b'a' + (state % 26) as u8)
        })
        .collect()
}

fn log_contents(log: &mut std::fs::File) -> String {
    log.rewind().expect("child log position");
    let mut text = String::new();
    log.read_to_string(&mut text).expect("child log");
    text
}

fn run_child(path: &Path, phase: &str, expected: i32) {
    // A slow-query log can include the overflow INSERT. File-backed output
    // keeps logging from blocking on a full pipe while the parent polls exit.
    let mut log = tempfile::tempfile().expect("child output");
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, path)
        .env("REDDB_LEGACY_GENERATION_PHASE", phase)
        .stdout(Stdio::from(log.try_clone().expect("stdout log")))
        .stderr(Stdio::from(log.try_clone().expect("stderr log")))
        .spawn()
        .expect("migration child");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("child status").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("stop stalled migration");
            child.wait().expect("stalled child termination");
            panic!("migration stalled: {phase}: {}", log_contents(&mut log));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().expect("migration result");
    assert_eq!(
        status.code(),
        Some(expected),
        "{phase}: {}",
        log_contents(&mut log)
    );
}

fn assert_rows(runtime: &RedDBRuntime, expected: &str, spill: bool) {
    let point = runtime
        .execute_query("SELECT payload FROM records WHERE id = 1")
        .expect("cold HASH lookup");
    assert_eq!(point.result.records.len(), 1);
    assert_eq!(
        point.result.records[0].get("payload"),
        Some(&Value::text(expected))
    );
    for sql in [
        "SELECT id,payload FROM records WHERE payload >= 'a' AND payload < 'z' ORDER BY id",
        "SELECT id,payload FROM records ORDER BY id",
    ] {
        let rows = runtime
            .execute_query(sql)
            .expect("BTREE and authoritative scan");
        assert_eq!(rows.result.records.len(), 2);
        for (row, (id, value)) in rows.result.records.iter().zip([(1, expected), (2, "keep")]) {
            assert_eq!(row.get("id"), Some(&Value::Integer(id)));
            assert_eq!(row.get("payload"), Some(&Value::text(value)));
        }
    }
    let result = runtime
        .execute_query("SELECT payload FROM spills WHERE id = 1")
        .expect("cold overflow row");
    assert_eq!(result.result.records.len(), usize::from(spill));
    if spill {
        assert!(
            result.result.records[0].get("payload") == Some(&Value::text(payload())),
            "overflow contents changed"
        );
    }
    assert!(runtime
        .verify_persisted_indexes("records")
        .expect("index verification")
        .is_empty());
}

fn owned_pages(path: &Path, protected: bool) -> HashSet<u32> {
    let pager = std::sync::Arc::new(
        Pager::open(
            path,
            PagerConfig {
                read_only: true,
                create: false,
                disk_residency: true,
                ..PagerConfig::default()
            },
        )
        .expect("inspect published ownership"),
    );
    let catalog = pager.read_manifest_page().expect("catalog");
    let header = reddb_file::decode_native_paged_metadata_header(catalog.content())
        .expect("catalog header")
        .expect("native catalog");
    let mut pos = reddb_file::METADATA_HEADER_BYTES;
    let mut roots = Vec::new();
    for _ in 0..header.collection_count {
        roots.push(
            reddb_file::decode_native_paged_collection_root(catalog.content(), &mut pos)
                .expect("root")
                .root_page,
        );
    }
    let count = u32::from_le_bytes(
        catalog.content()[pos..pos + 4]
            .try_into()
            .expect("reference count"),
    );
    pos += 4;
    for _ in 0..count {
        reddb_file::decode_native_paged_cross_ref(catalog.content(), &mut pos).expect("reference");
    }
    let retired = reddb_file::decode_native_paged_retired_trees(catalog.content(), &mut pos)
        .expect("retired roots");
    roots.extend(retired);
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .expect("resume")
        .expect("prepared legacy catalog");
    for state in resume.collections.values() {
        roots.extend(state.key_index_root);
        roots.extend(
            state
                .secondary_indices
                .values()
                .map(|index| index.root_page),
        );
    }
    let proofs =
        reddb_file::native_store::decode_native_paged_catalog_tree_proofs(catalog.content())
            .expect("proof trailer");
    if protected {
        assert!(
            proofs.is_some(),
            "first protected publication needs independent proofs"
        );
    } else {
        assert!(
            proofs.is_none(),
            "fixture must begin without proof authority"
        );
    }
    let mut owned = HashSet::new();
    for root in roots.into_iter().filter(|root| *root != 0) {
        let tree = if let Some(proofs) = &proofs {
            let proof = *proofs.get(&root).unwrap_or_else(|| {
                panic!("published legacy root {root} has no independent generation proof")
            });
            BTree::with_verified_root(std::sync::Arc::clone(&pager), root, proof)
                .expect("sealed tree")
        } else {
            BTree::with_root(std::sync::Arc::clone(&pager), root)
        };
        for page in tree.retired_pages().expect("complete owned generation") {
            assert!(
                owned.insert(page),
                "two generations own physical page {page}"
            );
        }
    }
    owned
}

fn assert_missing_retired_proof_is_rejected(path: &Path) {
    let original = std::fs::read(path).expect("published migration image");
    let mut bytes = original.clone();
    let selected = reddb_file::select_paged_superblock(&bytes).expect("selected superblock");
    let start = selected.copy_index * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
    let reference = reddb_file::file_format::database_header_manifest_reference(
        &bytes[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE],
    )
    .expect("published header");
    let offset = reference.page_id as usize * PAGE_SIZE;
    let mut page = Page::from_slice(&bytes[offset..offset + PAGE_SIZE]).expect("catalog page");
    assert!(
        reddb_file::decode_native_metadata_overflow_header(page.content())
            .expect("catalog framing")
            .is_none()
    );
    let mut data = page.content().to_vec();
    let catalog = reddb_file::decode_native_paged_metadata_header(&data)
        .expect("catalog header")
        .expect("native format");
    let mut pos = reddb_file::METADATA_HEADER_BYTES;
    for _ in 0..catalog.collection_count {
        reddb_file::decode_native_paged_collection_root(&data, &mut pos).expect("collection");
    }
    let count = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("reference count"));
    pos += 4;
    for _ in 0..count {
        reddb_file::decode_native_paged_cross_ref(&data, &mut pos).expect("reference");
    }
    let retired =
        reddb_file::decode_native_paged_retired_trees(&data, &mut pos).expect("retired roots");
    let root = *retired
        .first()
        .expect("first migration retains legacy trees");
    reddb_file::decode_native_paged_deferred_pages(&data, &mut pos).expect("deferred ownership");
    reddb_file::native_store::decode_native_paged_ownership_epoch(&data, &mut pos)
        .expect("catalog epoch");
    let proof_start = pos;
    let mut proofs = reddb_file::page_proof::decode_catalog(&data, &mut pos)
        .expect("proofs")
        .expect("protected publication");
    assert!(proofs.remove(&root).is_some());
    let mut encoded = Vec::new();
    reddb_file::page_proof::encode_catalog(&mut encoded, &proofs)
        .expect("encode incomplete proof map");
    data.splice(proof_start..pos, encoded);
    page.content_mut().fill(0);
    page.content_mut()[..data.len()].copy_from_slice(&data);
    page.update_checksum();
    bytes[offset..offset + PAGE_SIZE].copy_from_slice(page.as_bytes());
    // Re-seal both catalog references: the malformed generation must fail its
    // ownership contract even though page CRCs and catalog authority are valid.
    for copy in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
        let start = copy * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
        let slot = &mut bytes[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE];
        let reference = reddb_file::page_proof::PageReference {
            digest: reddb_file::page_proof::page_digest(page.as_bytes()),
            ..reference
        };
        reddb_file::file_format::set_database_header_manifest_reference(slot, reference)
            .expect("new catalog digest");
        reddb_file::seal_paged_superblock_slot(slot, copy, selected.generation + copy as u64 + 1)
            .expect("seal malformed generation");
    }
    let bad = path.with_file_name("missing-retired-proof.rdb");
    std::fs::write(&bad, &bytes).expect("malformed publication fixture");
    let error = crate::storage::compact::scrub_paged_trees(&bad)
        .expect_err("offline reader requires retired authority");
    assert!(error.to_string().contains("generation proof"), "{error}");
    let error = crate::storage::UnifiedStore::open_with_config(
        &bad,
        crate::storage::UnifiedStoreConfig {
            resident_memory_bytes_max: Some(64 * 1024 * 1024),
            ..Default::default()
        },
    )
    .err()
    .expect("native reader requires retired authority");
    assert!(
        error
            .to_string()
            .contains("missing published tree generation proof"),
        "{error}"
    );
    assert!(
        std::fs::read(&bad).expect("rejected source") == bytes,
        "rejection changed malformed publication evidence"
    );
    assert!(std::fs::read(path).expect("original migration source") == original);
}

#[test]
fn legacy_native_migration_survives_crash_and_reclaims_verified_generations() {
    if let Some(path) = std::env::var_os(CHILD) {
        let path = Path::new(&path);
        let phase = std::env::var("REDDB_LEGACY_GENERATION_PHASE").expect("phase");
        let runtime = open(path).expect("cold native open");
        if phase == "before" || phase == "after" {
            assert_rows(&runtime, "original", true);
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
        } else if let Some(cycle) = phase.strip_prefix("cycle-") {
            let cycle: usize = cycle.parse().expect("cycle number");
            let previous = if cycle == 0 {
                "committed".to_string()
            } else {
                format!("cycle-{}", cycle - 1)
            };
            assert_rows(&runtime, &previous, cycle.is_multiple_of(2));
            runtime
                .execute_query("INSERT INTO records (id,payload) VALUES (1,'forbidden')")
                .expect_err("UNIQUE survives migration");
            runtime
                .execute_query(&format!(
                    "UPDATE records SET payload = 'cycle-{cycle}' WHERE id = 1"
                ))
                .expect("new committed write");
            runtime
                .execute_query("INSERT INTO records (id,payload) VALUES (3,'progress')")
                .expect("new indexed row");
            runtime
                .execute_query("DELETE FROM records WHERE id = 3")
                .expect("delete transient row");
            if cycle.is_multiple_of(2) {
                runtime
                    .execute_query("DELETE FROM spills")
                    .expect("retire overflow record");
            } else {
                runtime
                    .execute_query(&format!(
                        "INSERT INTO spills (id,payload) VALUES (1,'{}')",
                        payload()
                    ))
                    .expect("recreate overflow record");
            }
            runtime
                .execute_query("VACUUM records")
                .expect("vacuum MVCC versions");
            runtime
                .execute_query("VACUUM spills")
                .expect("vacuum overflow versions");
            assert_rows(
                &runtime,
                &format!("cycle-{cycle}"),
                !cycle.is_multiple_of(2),
            );
        } else {
            assert_rows(&runtime, "committed", true);
        }
        runtime.checkpoint().expect("publish migrated generation");
        std::process::exit(0);
    }
    for phase in ["after", "before", "one-slot"] {
        let directory = tempfile::tempdir().expect("migration fixture");
        let modern = directory.path().join("modern.rdb");
        seed(&modern);
        {
            let runtime = open(&modern).expect("seed overflow collection");
            runtime
                .execute_query("CREATE TABLE spills (id INT, payload TEXT)")
                .expect("overflow table");
            runtime
                .execute_query(&format!(
                    "INSERT INTO spills (id,payload) VALUES (1,'{}')",
                    payload()
                ))
                .expect("overflow row");
            runtime.graceful_shutdown(false).expect("seed checkpoint");
        }
        let legacy = directory.path().join("legacy.rdb");
        crate::storage::compact::legacy_fixture::rewrite(&modern, &legacy)
            .expect("construct pre-proof layout");
        let previous_pages = owned_pages(&legacy, false);
        run_child(
            &legacy,
            if phase == "after" { "after" } else { "before" },
            if phase == "after" { 198 } else { 197 },
        );
        if phase == "one-slot" {
            super::catalog_publication_faults::publish_one_slot(&legacy);
            let mut bytes = std::fs::read(&legacy).expect("partial publication");
            let newest = reddb_file::select_paged_superblock(&bytes).expect("newest slot");
            let start = newest.copy_index * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
            bytes[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE].fill(0);
            std::fs::write(&legacy, bytes).expect("lose the newest publication slot");
        }
        if phase == "before" {
            owned_pages(&legacy, false);
        }
        if phase == "after" {
            owned_pages(&legacy, true);
            assert_missing_retired_proof_is_rejected(&legacy);
        }
        run_child(&legacy, "verify", 0);
        owned_pages(&legacy, true);
        let mut reused = false;
        for cycle in 0..6 {
            run_child(&legacy, &format!("cycle-{cycle}"), 0);
            reused |= !owned_pages(&legacy, true).is_disjoint(&previous_pages);
        }
        assert!(
            reused,
            "retired legacy pages must become reusable after publication fences"
        );
    }
}
