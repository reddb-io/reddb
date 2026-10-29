use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Instant;

use reddb_file::{NativeFulltextFrame, NativeFulltextPosting, NativeFulltextTerm};
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use reddb_server::storage::EntityData;
use reddb_types::Value;

const TABLE: &str = "scaling_rows";

fn files(path: &Path) -> Vec<serde_json::Value> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(path).expect("fixture directory") {
        let entry = entry.expect("fixture entry");
        let metadata = entry.metadata().expect("fixture metadata");
        if metadata.is_dir() {
            result.extend(files(&entry.path()));
        } else {
            result.push(serde_json::json!({
                "path": entry.path(), "logical_bytes": metadata.len(),
                "allocated_bytes": metadata.blocks() * 512
            }));
        }
    }
    result.sort_by_key(|item| item["path"].to_string());
    result
}

fn verify_frame(runtime: &RedDBRuntime) -> usize {
    let store = runtime.db().store();
    let header = store.physical_file_header().expect("published header");
    let (_, actual) = store
        .read_native_vector_artifact_blob(header.vector_artifact_page, TABLE, Some("text.fulltext"))
        .expect("read published fulltext")
        .expect("fulltext exists");
    let manager = store.get_collection(TABLE).expect("source manager");
    let mut terms = BTreeMap::<String, Vec<NativeFulltextPosting>>::new();
    let mut doc_count = 0;
    manager.for_each_entity(|entity| {
        let EntityData::Row(row) = &entity.data else {
            panic!("only table rows");
        };
        let text = row
            .named
            .iter()
            .flat_map(|named| named.values())
            .chain(row.columns.iter())
            .map(|value| match value {
                Value::Text(text) => text.to_string(),
                Value::Integer(number) => number.to_string(),
                _ => panic!("fixture uses only text and integer fields"),
            })
            .collect::<Vec<_>>()
            .join(" ");
        if !text.trim().is_empty() {
            doc_count += 1;
            let mut frequencies = BTreeMap::<String, u32>::new();
            for term in text
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|term| term.len() >= 2)
            {
                *frequencies.entry(term.to_owned()).or_default() += 1;
            }
            for (term, term_count) in frequencies {
                terms.entry(term).or_default().push(NativeFulltextPosting {
                    entity_id: entity.id.raw(),
                    term_count,
                });
            }
        }
        true
    });
    let expected = NativeFulltextFrame {
        collection: TABLE.to_owned(),
        doc_count,
        terms: terms
            .into_iter()
            .map(|(term, mut postings)| {
                postings.sort_unstable_by_key(|posting| posting.entity_id);
                NativeFulltextTerm { term, postings }
            })
            .collect(),
    };
    assert_eq!(
        actual,
        reddb_file::encode_native_fulltext_frame(&expected),
        "delta must equal full derivation of all physical source versions"
    );
    actual.len()
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("reddb::checkpoint=debug")
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    let root = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("usage: fulltext-scaling <new-synthetic-fixture-directory>"),
    );
    std::fs::create_dir(&root).expect("fresh preserved synthetic fixture directory");
    for rows in [256, 2048, 8192] {
        let directory = root.join(rows.to_string());
        std::fs::create_dir(&directory).expect("fresh size fixture");
        let path = directory.join("database.rdb");
        let options = RedDBOptions::persistent(&path)
            .with_memory_budget(512 * 1024 * 1024)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile");
        let runtime = RedDBRuntime::with_options(options.clone()).expect("runtime");
        for sql in [
            "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
            "SET CONFIG runtime.result_cache.enabled = false",
            "CREATE TABLE scaling_rows (id INT,payload TEXT)",
            "CREATE UNIQUE INDEX scaling_rows_id ON scaling_rows (id)",
        ] {
            runtime.execute_query(sql).expect("fixture schema");
        }
        for start in (0..rows).step_by(128) {
            let values = (start..(start + 128).min(rows))
                .map(|id| format!("({id},'common seed{id:08} alpha beta gamma delta epsilon')"))
                .collect::<Vec<_>>()
                .join(",");
            runtime
                .execute_query(&format!(
                    "INSERT INTO scaling_rows (id,payload) VALUES {values}"
                ))
                .expect("seed batch");
        }
        runtime
            .db()
            .store()
            .get_collection(TABLE)
            .expect("manager")
            .force_seal()
            .expect("seal baseline");
        for cycle in 0..=3 {
            let mut changed_payload_bytes = 0;
            if cycle > 0 {
                for id in 0..6 {
                    let payload =
                        format!("common edit{cycle:08} row{id:08} alpha beta gamma delta");
                    changed_payload_bytes += payload.len();
                    runtime
                        .execute_query_with_params(
                            &format!("UPDATE scaling_rows SET payload=$1 WHERE id={id}"),
                            &[Value::text(payload)],
                        )
                        .expect("six equivalent row edits");
                }
            }
            eprintln!("PROBE_BEGIN rows={rows} cycle={cycle}");
            let pager = runtime.db().store().pager().expect("pager").clone();
            let before = pager.cache_stats();
            let started = Instant::now();
            runtime.checkpoint().expect("checkpoint");
            let elapsed = started.elapsed().as_secs_f64();
            let after = pager.cache_stats();
            let page_count = pager.page_count().expect("total pages");
            let free_page_count = pager.free_page_count().expect("free pages");
            let references = runtime
                .db()
                .native_vector_artifact_pages()
                .expect("artifact references");
            let text_reference = references
                .iter()
                .find(|reference| {
                    reference.collection == TABLE && reference.artifact_kind == "text.fulltext"
                })
                .expect("text reference");
            let bytes = verify_frame(&runtime);
            let count = runtime
                .execute_query("SELECT COUNT(*) AS n FROM scaling_rows")
                .expect("live count");
            assert_eq!(
                count.result.records[0].get("n").expect("count").to_string(),
                rows.to_string()
            );
            if cycle > 0 {
                let result = runtime
                    .execute_query("SELECT id,payload FROM scaling_rows WHERE id=5")
                    .expect("indexed updated payload");
                assert_eq!(result.result.records.len(), 1);
                assert_eq!(
                    result.result.records[0]
                        .get("payload")
                        .expect("payload")
                        .as_text(),
                    Some(
                        format!("common edit{cycle:08} row00000005 alpha beta gamma delta")
                            .as_str()
                    )
                );
            }
            println!(
                "{}",
                serde_json::json!({"rows": rows, "cycle": cycle, "changed_rows": if cycle == 0 {0} else {6}, "changed_payload_bytes": changed_payload_bytes, "checkpoint_seconds": elapsed, "fulltext_logical_bytes": bytes, "fulltext_reference": text_reference, "page_count": page_count, "free_page_count": free_page_count, "pager_accesses": after.hits + after.misses - before.hits - before.misses, "files": files(&directory), "full_derivation_equal": true})
            );
            eprintln!("PROBE_END rows={rows} cycle={cycle}");
        }
        drop(runtime);
        let reopened = RedDBRuntime::with_options(options).expect("cold recovery");
        let count = reopened
            .execute_query("SELECT COUNT(*) AS n FROM scaling_rows")
            .expect("recovered count");
        assert_eq!(
            count.result.records[0].get("n").expect("count").to_string(),
            rows.to_string()
        );
        let result = reopened
            .execute_query("SELECT payload FROM scaling_rows WHERE id=5")
            .expect("recovered edited payload");
        assert_eq!(
            result.result.records[0]
                .get("payload")
                .expect("payload")
                .as_text(),
            Some("common edit00000003 row00000005 alpha beta gamma delta")
        );
        // Named rows persist their authoritative named representation; the raw
        // positional columns observed by artifact derivation can differ after
        // reopen. Source-proof admission must reject and rebuild that cache.
        // Check data before publication, then require exact derived equality
        // after the first cold publication rather than blessing stale bytes.
        eprintln!("PROBE_BEGIN rows={rows} cycle=cold");
        let started = Instant::now();
        reopened.checkpoint().expect("validate/rebuild cold source cache");
        let cold_checkpoint_seconds = started.elapsed().as_secs_f64();
        verify_frame(&reopened);
        eprintln!("PROBE_END rows={rows} cycle=cold");
        println!(
            "{}",
            serde_json::json!({"rows": rows, "cold_recovery": true, "cold_checkpoint_seconds": cold_checkpoint_seconds, "full_derivation_equal": true})
        );
    }
}
