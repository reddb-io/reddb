//! Opt-in qualification over an independently preserved historical copy. Never
//! selected by the normal suite or the managed runtime's storage factory.
use crate::json::Value as JsonValue;
use crate::storage::{UnifiedStore, UnifiedStoreConfig};
use std::time::Instant;

/// Phase-boundary observations only; no per-row instrumentation. CPU is scoped
/// to the qualifier's cgroup, while I/O counters belong to the test process.
#[derive(Default)]
struct PhaseResources {
    cpu_usage_us: Option<u64>,
    read_bytes: Option<u64>,
    read_chars: Option<u64>,
    read_calls: Option<u64>,
    write_bytes: Option<u64>,
}

impl PhaseResources {
    fn capture() -> Self {
        Self::parse(
            std::fs::read_to_string("/sys/fs/cgroup/cpu.stat")
                .ok()
                .as_deref(),
            std::fs::read_to_string("/proc/self/io").ok().as_deref(),
        )
    }

    fn parse(cpu: Option<&str>, io: Option<&str>) -> Self {
        Self {
            cpu_usage_us: cpu.and_then(|text| resource_counter(text, "usage_usec")),
            read_bytes: io.and_then(|text| resource_counter(text, "read_bytes")),
            read_chars: io.and_then(|text| resource_counter(text, "rchar")),
            read_calls: io.and_then(|text| resource_counter(text, "syscr")),
            write_bytes: io.and_then(|text| resource_counter(text, "write_bytes")),
        }
    }

    fn delta(&self, after: &Self) -> JsonValue {
        let difference = |before: Option<u64>, after: Option<u64>| {
            before
                .zip(after)
                .and_then(|(before, after)| after.checked_sub(before))
                .map(|value| {
                    i64::try_from(value)
                        .map(JsonValue::Integer)
                        .unwrap_or_else(|_| JsonValue::Decimal(value.to_string()))
                })
                .unwrap_or(JsonValue::Null)
        };
        crate::json::json!({
            "cgroup_cpu_usage_us": difference(self.cpu_usage_us, after.cpu_usage_us),
            "process_read_bytes": difference(self.read_bytes, after.read_bytes),
            "process_read_chars": difference(self.read_chars, after.read_chars),
            "process_read_calls": difference(self.read_calls, after.read_calls),
            "process_write_bytes": difference(self.write_bytes, after.write_bytes),
        })
    }

    fn report(&self, step: &str, pass: usize, name: &str) {
        let after = Self::capture();
        eprintln!("runtime qualification phase=resources step={step} pass={pass} name={name:?} counters={}",
            self.delta(&after).to_string_compact());
    }
}

fn resource_counter(text: &str, name: &str) -> Option<u64> {
    let mut found = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else { continue };
        if key.trim_end_matches(':') != name {
            continue;
        }
        let value = fields.next()?.parse::<u64>().ok()?;
        if fields.next().is_some() || found.replace(value).is_some() {
            return None;
        }
    }
    found
}

#[test]
fn historical_resource_observations_preserve_unknown_and_reset_counters() {
    let before = PhaseResources::parse(
        Some("usage_usec 100\nuser_usec 50\n"),
        Some("rchar: 300\nsyscr: 10\nread_bytes: 4096\nwrite_bytes: 0\n"),
    );
    let after = PhaseResources::parse(
        Some("usage_usec 160\n"),
        Some("rchar: 300\nsyscr: 12\nread_bytes: 8192\nwrite_bytes: 4096\n"),
    );
    assert_eq!(
        crate::json::from_str::<JsonValue>(&before.delta(&after).to_string_compact())
            .expect("resource JSON"),
        crate::json::from_str::<JsonValue>(
            r#"{
            "cgroup_cpu_usage_us": 60,
            "process_read_chars": 0,
            "process_read_calls": 2,
            "process_read_bytes": 4096,
            "process_write_bytes": 4096
        }"#
        )
        .expect("independent counters")
    );
    assert_eq!(after.delta(&before)["process_read_bytes"], JsonValue::Null);
    assert_eq!(
        before.delta(&PhaseResources::default())["cgroup_cpu_usage_us"],
        JsonValue::Null
    );
    assert_eq!(resource_counter("read_bytes: 0\n", "read_bytes"), Some(0));
    assert_eq!(
        resource_counter("\nread_bytes: 0\n\n", "read_bytes"),
        Some(0)
    );
    let large = PhaseResources {
        read_bytes: Some(u64::MAX),
        ..Default::default()
    };
    let zero = PhaseResources {
        read_bytes: Some(0),
        ..Default::default()
    };
    let parsed: JsonValue = crate::json::from_str(&zero.delta(&large).to_string_compact())
        .expect("lossless u64 counter");
    assert_eq!(parsed["process_read_bytes"].as_u64(), Some(u64::MAX));
    for invalid in [
        "read_bytes: -1",
        "read_bytes: bad",
        "read_bytes: 2 extra",
        "read_bytes: 1\nread_bytes: 2",
    ] {
        assert_eq!(resource_counter(invalid, "read_bytes"), None);
    }
}

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

fn runtime_index_descriptors(runtime: &crate::runtime::RedDBRuntime) -> JsonValue {
    use crate::runtime::QualificationIndexMethodKind as IndexMethodKind;

    let mut indices = Vec::new();
    for collection in runtime.db().store().list_collections() {
        for index in runtime.index_store_ref().list_indices(&collection) {
            let (method, resolution) = match index.method {
                IndexMethodKind::Hash => ("hash", 0),
                IndexMethodKind::Bitmap => ("bitmap", 0),
                IndexMethodKind::BTree => ("btree", 0),
                IndexMethodKind::H3 { resolution } => ("h3", resolution),
            };
            indices.push(crate::json::json!({
                "collection": index.collection,
                "name": index.name,
                "columns": index.columns,
                "method": method,
                "unique": index.unique,
                "resolution": JsonValue::Integer(i64::from(resolution)),
            }));
        }
    }
    indices.sort_by(|left, right| {
        (left["collection"].as_str(), left["name"].as_str())
            .cmp(&(right["collection"].as_str(), right["name"].as_str()))
    });
    JsonValue::Array(indices)
}

#[test]
fn historical_index_reference_preserves_unique_method_and_column_order() {
    let runtime = crate::runtime::RedDBRuntime::with_options(
        crate::api::RedDBOptions::in_memory().with_memory_budget(64 * 1024 * 1024),
    )
    .expect("index reference runtime");
    runtime
        .execute_query("CREATE TABLE reference_rows (account_id INTEGER, item TEXT)")
        .expect("table");
    runtime
        .execute_query(
            "CREATE UNIQUE INDEX reference_tuple ON reference_rows (item, account_id) USING BTREE",
        )
        .expect("composite unique index");
    assert_eq!(
        runtime_index_descriptors(&runtime),
        crate::json::from_str::<JsonValue>(
            r#"[{
            "collection": "reference_rows",
            "name": "reference_tuple",
            "columns": ["item", "account_id"],
            "method": "btree",
            "unique": true,
            "resolution": 0
        }]"#
        )
        .expect("independent descriptor JSON")
    );
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
    let index_reference = std::env::var_os("REDDB_QUALIFICATION_INDEX_REFERENCE")
        .expect("independent index descriptor reference path");
    let index_reference = std::fs::read_to_string(index_reference).expect("index reference");
    let index_reference: JsonValue =
        crate::json::from_str(&index_reference).expect("index reference JSON");
    assert!(
        !index_reference["indices"]
            .as_array()
            .expect("expected index descriptors")
            .is_empty(),
        "a zero-index substitute cannot qualify the real historical database"
    );
    let eager_control = match std::env::var("REDDB_QUALIFICATION_RESIDENCY_MODE").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("bounded") => false,
        Ok("eager-control") => true,
        _ => panic!("explicit bounded or eager-control qualification mode required"),
    };
    let memory_budget = if eager_control {
        // Reproduce the serving Platform's current budget on an independent
        // historical copy. This control cannot qualify bounded residency.
        let limit = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
            .expect("eager control requires an external cgroup limit");
        assert_eq!(limit.trim(), "5368709120", "eager control requires 5 GiB");
        3584 * 1024 * 1024
    } else {
        768 * 1024 * 1024
    };
    eprintln!(
        "runtime qualification phase=configuration eager_control={eager_control} memory_budget_bytes={memory_budget}"
    );
    let options = RedDBOptions::persistent(path)
        .with_memory_budget(memory_budget)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
        .expect("profile");
    for pass in 1..=2 {
        let resources = PhaseResources::capture();
        let started = Instant::now();
        let runtime = if eager_control {
            RedDBRuntime::with_options(options.clone())
        } else {
            crate::storage::RedDB::with_disk_residency_for_qualification(|| {
                RedDBRuntime::with_options(options.clone())
            })
        }
        .expect("complete runtime opens the historical copy");
        assert_eq!(
            runtime
                .db()
                .store()
                .config()
                .resident_memory_bytes_max
                .is_some(),
            !eager_control,
            "the eager control must not open an already migrated bounded base"
        );
        eprintln!(
            "runtime qualification phase=open pass={pass} open_ms={} resident_segment_bytes={}",
            started.elapsed().as_millis(),
            runtime.db().store().segment_memory_bytes(),
        );
        resources.report("open", pass, "runtime");
        eprintln!(
            "runtime qualification phase=boot_markers pass={pass} markers={:?}",
            runtime.lifecycle().cold_start_phases().durations_ms(),
        );
        let store = runtime.db().store();
        let source_collections = index_reference["sourceCollections"]
            .as_array()
            .expect("independent complete physical collection list");
        assert!(
            !source_collections.is_empty(),
            "never qualify a partial substitute"
        );
        for collection in source_collections {
            let name = collection.as_str().expect("source collection name");
            assert!(
                store.get_collection(name).is_some(),
                "source collection {name} survives"
            );
        }
        for orphan in index_reference["orphanedDescriptors"]
            .as_array()
            .expect("explicitly audited orphan descriptor list")
        {
            let name = orphan["collection"]
                .as_str()
                .expect("absent original collection");
            assert!(
                store.get_collection(name).is_none(),
                "orphan reference cannot hide a live collection {name}"
            );
        }
        let indices = runtime_index_descriptors(&runtime);
        assert_eq!(
            indices, index_reference["indices"],
            "all declared index descriptors survive runtime construction, pass {pass}"
        );
        eprintln!(
            "runtime qualification phase=indices pass={pass} restored_indices={} index_memory_bytes={}",
            indices.as_array().expect("restored indices").len(),
            runtime.index_store_ref().memory_bytes(),
        );
        for case in cases {
            runtime.invalidate_result_cache();
            let resources = PhaseResources::capture();
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
            resources.report("query", pass, case["name"].as_str().expect("case name"));
        }
        let resources = PhaseResources::capture();
        let started = Instant::now();
        runtime.flush().expect("historical runtime checkpoint");
        eprintln!(
            "runtime qualification phase=checkpoint pass={pass} checkpoint_ms={} resident_segment_bytes={}",
            started.elapsed().as_millis(),
            runtime.db().store().segment_memory_bytes(),
        );
        resources.report("checkpoint", pass, "runtime");
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
