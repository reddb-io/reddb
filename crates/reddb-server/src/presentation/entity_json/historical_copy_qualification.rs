//! Opt-in qualification over an independently preserved historical copy. Never
//! selected by the normal suite or the managed runtime's storage factory.
use crate::json::Value as JsonValue;
use crate::storage::{UnifiedStore, UnifiedStoreConfig};
use std::time::Instant;

fn sql_projection(result: &crate::storage::query::unified::UnifiedResult) -> JsonValue {
    let rendered = crate::presentation::query_result_json::unified_result_json_with_records(
        result,
        &result.records,
    );
    // The independent SQL reference contains each HTTP record's complete
    // projected values map, not its multimodel envelope. Preserve every field
    // and use the same transport round trip to normalize JSON numbers.
    let rows = JsonValue::Array(
        rendered["records"]
            .as_array()
            .expect("rendered records")
            .iter()
            .map(|record| record["values"].clone())
            .collect(),
    );
    crate::json::from_str(&rows.to_string_compact()).expect("rendered SQL values")
}

#[test]
fn historical_sql_reference_matches_projected_http_values() {
    let runtime = crate::runtime::RedDBRuntime::with_options(
        crate::api::RedDBOptions::in_memory().with_memory_budget(64 * 1024 * 1024),
    )
    .expect("reference-format runtime");
    let result = runtime
        .execute_query("SELECT 178120 AS n, 'reference' AS id, NULL AS optional")
        .expect("reference projection");
    let expected: JsonValue =
        crate::json::from_str(r#"[{"n":178120,"id":"reference","optional":null}]"#)
            .expect("independent flat SQL reference");
    assert_eq!(sql_projection(&result.result), expected);
}

#[test]
#[ignore = "requires an owned historical copy, independent SQL results and an external cgroup limit"]
fn complete_runtime_historical_copy_preserves_sql_results() {
    use crate::api::RedDBOptions;
    use crate::runtime::RedDBRuntime;
    use crate::storage::profile::StorageDeployPreset;

    let root = std::env::var_os("REDDB_QUALIFICATION_COPY_DIR").expect("owned copy directory");
    let root = std::fs::canonicalize(root).expect("existing copy");
    let marker = std::fs::read_to_string(root.join(".reddb-owned-qualification-copy"))
        .expect("explicit ownership marker before opening mutable native files");
    assert_eq!(marker.trim(), "disk-residency-historical-qualification-v1");
    let path = root.join("reddb.rdb");
    assert!(path.is_file(), "never create an empty substitute database");
    let reference = std::env::var_os("REDDB_QUALIFICATION_SQL_REFERENCE")
        .expect("independent SQL reference path");
    let reference = std::fs::read_to_string(reference).expect("SQL reference");
    let reference: JsonValue = crate::json::from_str(&reference).expect("reference JSON");
    let cases = reference.as_array().expect("SQL cases");
    assert_eq!(cases.len(), 10, "complete Platform diagnostic workload");
    let options = RedDBOptions::persistent(path)
        .with_memory_budget(768 * 1024 * 1024)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
        .expect("profile");
    for pass in 1..=2 {
        let started = Instant::now();
        let runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options.clone())
        })
        .expect("complete runtime opens the historical copy");
        assert!(runtime
            .db()
            .store()
            .config()
            .resident_memory_bytes_max
            .is_some());
        eprintln!(
            "runtime qualification phase=open pass={pass} open_ms={} resident_segment_bytes={}",
            started.elapsed().as_millis(),
            runtime.db().store().segment_memory_bytes(),
        );
        for case in cases {
            runtime.invalidate_result_cache();
            let started = Instant::now();
            let result = runtime
                .execute_query(case["sql"].as_str().expect("SQL"))
                .expect("historical SQL succeeds");
            let actual = sql_projection(&result.result);
            assert_eq!(actual, case["rows"], "pass {pass}, {}", case["name"]);
            eprintln!(
                "runtime qualification phase=query pass={pass} name={} elapsed_ms={}",
                case["name"],
                started.elapsed().as_millis(),
            );
        }
        let started = Instant::now();
        runtime.flush().expect("historical runtime checkpoint");
        eprintln!(
            "runtime qualification phase=checkpoint pass={pass} checkpoint_ms={} resident_segment_bytes={}",
            started.elapsed().as_millis(),
            runtime.db().store().segment_memory_bytes(),
        );
        drop(runtime);
    }
}

fn fingerprint(store: &UnifiedStore, expected: &JsonValue) -> u64 {
    let capture = std::env::var_os("REDDB_QUALIFICATION_MISMATCH_CAPTURE");
    let collections = expected["collections"]
        .as_array()
        .expect("reference collections");
    let mut actual_names = store.list_collections();
    actual_names.sort();
    let mut expected_names: Vec<_> = collections
        .iter()
        .map(|item| item["name"].as_str().expect("collection name").to_string())
        .collect();
    expected_names.sort();
    assert_eq!(
        actual_names, expected_names,
        "all physical collections survive"
    );
    let mut total = 0;
    for (index, reference) in collections.iter().enumerate() {
        let name = reference["name"].as_str().expect("collection");
        let manager = store.get_collection(name).expect("physical manager");
        let mut count = 0u64;
        let mut sum = [0u8; 32];
        let mut xor = [0u8; 32];
        let mut samples = String::new();
        manager
            .try_scan_for_each(None, 16 * 1024 * 1024, |entity| {
                // The external API reference was produced by ordinary recovery,
                // which compacts eligible named maps into positional rows. Use
                // that exact projection, without retaining the decoded rows.
                // It preserves conflicting explicit positions and the bounded
                // per-collection schema diversity rather than sorting blindly.
                let mut entity = entity.clone();
                manager.restore_snapshot_row_layout(&mut entity);
                // This is the actual public native entity DTO used to produce
                // the independently exported reference. Its object maps are
                // ordered, and its integer/decimal lexemes stay lossless.
                let json = super::entity_json(&entity).to_string_compact();
                let digest = crate::crypto::sha256(json.as_bytes());
                if capture.is_some()
                    && count < 3
                    && samples.len().saturating_add(json.len()) < 1024 * 1024
                {
                    samples.push_str(&json);
                    samples.push('\n');
                }
                let mut carry = 0u16;
                for index in (0..32).rev() {
                    let value = u16::from(sum[index]) + u16::from(digest[index]) + carry;
                    sum[index] = value.to_le_bytes()[0];
                    carry = value >> 8;
                    xor[index] ^= digest[index];
                }
                count += 1;
                true
            })
            .expect("every physical read succeeds; no truncated prefix");
        assert_eq!(count, reference["count"].as_u64().expect("count"), "{name}");
        if hex::encode(sum) != reference["sha256Sum"].as_str().expect("sum") {
            if let Some(path) = &capture {
                use std::io::Write;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                options
                    .open(path)
                    .expect("new private mismatch capture")
                    .write_all(samples.as_bytes())
                    .expect("bounded private mismatch capture");
            }
        }
        assert_eq!(
            hex::encode(sum),
            reference["sha256Sum"].as_str().expect("sum"),
            "{name}"
        );
        assert_eq!(
            hex::encode(xor),
            reference["sha256Xor"].as_str().expect("xor"),
            "{name}"
        );
        total += count;
        println!(
            "qualification verified_collections={} verified_entities={total}",
            index + 1
        );
    }
    assert_eq!(total, expected["rows"].as_u64().expect("reference total"));
    total
}

#[test]
#[ignore = "requires an owned historical copy, its independent native fingerprint and an external cgroup limit"]
fn disk_resident_historical_copy_preserves_every_exported_entity() {
    let root = std::env::var_os("REDDB_QUALIFICATION_COPY_DIR").expect("owned copy directory");
    let root = std::fs::canonicalize(root).expect("existing copy");
    let marker = std::fs::read_to_string(root.join(".reddb-owned-qualification-copy"))
        .expect("explicit ownership marker is required before opening mutable native files");
    assert_eq!(marker.trim(), "disk-residency-historical-qualification-v1");
    let path = root.join("reddb.rdb");
    assert!(path.is_file(), "never create an empty substitute database");
    let reference = std::env::var_os("REDDB_QUALIFICATION_REFERENCE").expect("reference path");
    let reference = std::fs::read_to_string(reference).expect("independently exported reference");
    let expected: JsonValue = crate::json::from_str(&reference).expect("reference JSON");
    assert_eq!(expected["complete"].as_bool(), Some(true));
    let config = UnifiedStoreConfig {
        // 128 MiB of pager slots and 256 MiB of resident overlays. The external
        // 1-GiB limit measures all native metadata, scratch and allocator RSS too.
        page_cache_slots: Some(8192),
        resident_memory_bytes_max: Some(256 * 1024 * 1024),
        ..UnifiedStoreConfig::default()
    };
    for phase in ["initial", "prepared"] {
        let started = Instant::now();
        let store =
            UnifiedStore::open_with_config(&path, config.clone()).expect("cold native open");
        println!(
            "qualification phase={phase} open_ms={}",
            started.elapsed().as_millis()
        );
        let rows = fingerprint(&store, &expected);
        assert!(store.segment_memory_bytes() <= 256 * 1024 * 1024);
        let checkpoint = Instant::now();
        store.persist().expect("durable generation publication");
        println!(
            "qualification phase={phase} verified_entities={rows} resident_segment_bytes={} checkpoint_ms={}",
            store.segment_memory_bytes(),
            checkpoint.elapsed().as_millis()
        );
    }
}
