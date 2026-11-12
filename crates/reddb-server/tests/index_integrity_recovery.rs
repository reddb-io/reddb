//! Detect damaged trees in closed fixtures; recover only verified source rows.
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use reddb_server::storage::compact::{salvage_paged_indexes, scrub_paged};
use reddb_server::storage::engine::{Pager, PagerConfig, PAGE_SIZE};
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn open(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(
        RedDBOptions::persistent(path)
            .with_memory_budget(64 * 1024 * 1024)
            .with_bounded_disk_residency(true),
    )
    .expect("open runtime")
}

fn seed(path: &Path, method: &str) {
    let runtime = open(path);
    for sql in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false".to_string(),
        "CREATE TABLE records (id INTEGER, org_id TEXT, payload TEXT)".to_string(),
        format!("CREATE INDEX by_org ON records (org_id) USING {method}"),
        "INSERT INTO records (id,org_id,payload) VALUES (1,'org-a','old'),(2,'org-b','keep'),(3,'org-a','deleted')".to_string(),
        "BEGIN".to_string(),
        "UPDATE records SET payload = 'current' WHERE id = 1".to_string(),
        "DELETE FROM records WHERE id = 3".to_string(),
        "COMMIT".to_string(),
    ] {
        runtime.execute_query(&sql).expect(&sql);
    }
    runtime
        .graceful_shutdown(false)
        .expect("checkpoint and close");
}

fn index_root(path: &Path) -> u32 {
    roots(path).1
}

fn roots(path: &Path) -> (u32, u32) {
    let state = catalog_state(path);
    (state.root_page, state.secondary_indices["by_org"].root_page)
}

fn catalog_state(path: &Path) -> reddb_file::native_resume::CollectionResume {
    let pager = Pager::open(
        path,
        PagerConfig {
            read_only: true,
            create: false,
            ..PagerConfig::default()
        },
    )
    .expect("read catalog");
    let catalog = pager.read_page(1).expect("catalog page");
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .expect("decode catalog")
        .expect("resume catalog");
    resume.collections["records"].clone()
}

fn damage_page(path: &Path, root: u32) {
    assert_ne!(root, 0);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("fault target");
    file.seek(SeekFrom::Start(u64::from(root) * PAGE_SIZE as u64 + 100))
        .expect("page offset");
    let mut byte = [0];
    file.read_exact(&mut byte).expect("original byte");
    file.seek(SeekFrom::Current(-1)).expect("rewind");
    file.write_all(&[byte[0] ^ 1])
        .expect("inject checksum failure");
    file.sync_all().expect("durable fault");
}

fn verify_rows(path: &Path, method: &str) {
    for _ in 0..2 {
        let runtime = open(path);
        // The first SQL read after open must exercise the damaged/rebuilt index.
        let predicate = if method == "HASH" {
            "org_id = 'org-a'"
        } else {
            "org_id >= 'org-a' AND org_id < 'org-b'"
        };
        let indexed = runtime
            .execute_query(&format!("SELECT id,payload FROM records WHERE {predicate}"))
            .expect("cold index read");
        assert_eq!(indexed.result.records.len(), 1);
        assert_eq!(
            indexed.result.records[0].get("id"),
            Some(&Value::Integer(1))
        );
        assert_eq!(
            indexed.result.records[0].get("payload"),
            Some(&Value::text("current"))
        );
        assert!(runtime
            .verify_persisted_indexes("records")
            .expect("restored index must be complete, not just a scan fallback")
            .is_empty());
        let scan = runtime
            .execute_query("SELECT id,org_id,payload FROM records ORDER BY id")
            .expect("all authoritative rows");
        assert_eq!(
            scan.result.records.len(),
            2,
            "deleted row must stay deleted"
        );
        for (row, (id, org, payload)) in scan
            .result
            .records
            .iter()
            .zip([(1, "org-a", "current"), (2, "org-b", "keep")])
        {
            assert_eq!(row.get("id"), Some(&Value::Integer(id)));
            assert_eq!(row.get("org_id"), Some(&Value::text(org)));
            assert_eq!(row.get("payload"), Some(&Value::text(payload)));
        }
        runtime
            .graceful_shutdown(false)
            .expect("publish repaired roots");
    }
}

#[test]
fn checksum_damaged_index_pages_are_detected_and_rebuilt_without_row_loss() {
    for method in ["HASH", "BTREE"] {
        let directory = tempfile::tempdir().expect("fixture");
        let source = directory.path().join("source.rdb");
        seed(&source, method);
        assert!(scrub_paged(&source)
            .expect("healthy scrub")
            .trees
            .findings
            .is_empty());
        let root = index_root(&source);
        damage_page(&source, root);
        let damaged = std::fs::read(&source).expect("source evidence");
        let scrub = scrub_paged(&source).expect("detect damage");
        assert!(!scrub.assets_verified);
        assert_eq!(scrub.trees.findings.len(), 1);
        assert_eq!(scrub.trees.findings[0].tree, "secondary index by_org");
        let destination = directory.path().join("recovered");
        let report = salvage_paged_indexes(&source, &destination).expect("rebuild derived index");
        assert_eq!(report.indexes.len(), 1);
        assert_eq!(report.indexes[0].index, "by_org");
        assert_eq!(std::fs::read(&source).expect("preserved source"), damaged);
        let recovered = destination.join("source.rdb");
        verify_rows(&recovered, method);
        assert!(scrub_paged(&recovered)
            .expect("repaired scrub")
            .trees
            .findings
            .is_empty());
    }
}

#[test]
fn index_recovery_refuses_corrupt_authoritative_data() {
    let directory = tempfile::tempdir().expect("fixture");
    let source = directory.path().join("source.rdb");
    seed(&source, "BTREE");
    let (data_root, index_root) = roots(&source);
    damage_page(&source, data_root);
    damage_page(&source, index_root);
    let damaged = std::fs::read(&source).expect("preserve evidence");
    let findings = scrub_paged(&source)
        .expect("detect data corruption")
        .trees
        .findings;
    assert!(findings
        .iter()
        .any(|finding| finding.tree == "collection" && finding.collection == "records"));
    let destination = directory.path().join("recovered");
    let error = salvage_paged_indexes(&source, &destination)
        .expect_err("data loss cannot be repaired by rebuilding an index");
    assert!(
        error
            .to_string()
            .contains("cannot safely recover collection"),
        "{error}"
    );
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).expect("source retained"), damaged);
}

#[test]
fn missing_posting_with_valid_checksum_is_detected_and_rebuilt() {
    use reddb_server::storage::engine::BTree;
    use std::sync::Arc;
    for method in ["HASH", "BTREE"] {
        let directory = tempfile::tempdir().expect("fixture");
        let source = directory.path().join("source.rdb");
        seed(&source, method);
        {
            let runtime = open(&source);
            assert!(runtime
                .verify_persisted_indexes("records")
                .expect("healthy logical check")
                .is_empty());
            runtime
                .graceful_shutdown(false)
                .expect("close healthy runtime");
        }
        let root = index_root(&source);
        {
            let pager = Arc::new(
                Pager::open(
                    &source,
                    PagerConfig {
                        disk_residency: true,
                        create: false,
                        ..PagerConfig::default()
                    },
                )
                .expect("fault pager"),
            );
            let tree = BTree::with_root(Arc::clone(&pager), root);
            let mut cursor = tree.cursor_first().expect("posting cursor");
            let (key, _) = cursor
                .next_with_budget(64 * 1024)
                .expect("read posting")
                .expect("nonempty index");
            drop(cursor);
            assert!(tree.delete(&key).expect("remove one posting"));
            assert_eq!(tree.root_page_id(), root, "keep the catalog root valid");
            pager.sync().expect("persist valid checksums");
        }
        // A structurally sound tree can be logically incomplete.
        assert!(scrub_paged(&source)
            .expect("physical scrub")
            .trees
            .findings
            .is_empty());
        let runtime = open(&source);
        let findings = runtime
            .verify_persisted_indexes("records")
            .expect("logical scrub");
        assert_eq!(findings.len(), 1, "{method}: {findings:?}");
        assert_eq!(findings[0].index, "by_org");
        assert!(
            findings[0].reason.contains("missing or invalid posting"),
            "{findings:?}"
        );
        runtime
            .execute_query("DROP INDEX by_org ON records")
            .expect("discard derived index");
        runtime
            .execute_query(&format!(
                "CREATE INDEX by_org ON records (org_id) USING {method}"
            ))
            .expect("rebuild from authoritative rows");
        runtime.checkpoint().expect("publish rebuilt index");
        assert!(runtime
            .verify_persisted_indexes("records")
            .expect("verify repair")
            .is_empty());
        runtime
            .graceful_shutdown(false)
            .expect("close repaired database");
        drop(runtime);
        verify_rows(&source, method);
    }
}

#[test]
fn missing_persisted_index_tree_is_rebuilt_on_cold_open() {
    for method in ["HASH", "BTREE"] {
        let directory = tempfile::tempdir().expect("fixture");
        let source = directory.path().join("source.rdb");
        seed(&source, method);
        {
            let runtime = open(&source);
            // Remove only the derived tree. The durable SQL definition remains.
            runtime
                .db()
                .store()
                .unregister_secondary_index("records", "by_org");
            runtime
                .graceful_shutdown(false)
                .expect("publish missing derived tree");
        }
        assert!(!catalog_state(&source)
            .secondary_indices
            .contains_key("by_org"));
        verify_rows(&source, method);
        assert_ne!(
            index_root(&source),
            0,
            "cold boot must rebuild the missing tree"
        );
        let runtime = open(&source);
        assert!(runtime
            .verify_persisted_indexes("records")
            .expect("complete restored tree")
            .is_empty());
    }
}
