use reddb_file::{
    decode_database_header, paged_superblock_slot_generation, FILE_CAPABILITY_TEMPERATURE,
    PAGED_SUPERBLOCK_SLOT_COUNT, PAGED_SUPERBLOCK_SLOT_SIZE,
};
use reddb_server::storage::compact::compact_paged;
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use reddb_types::{Temperature, TemperatureUnit, Value};

fn execute(runtime: &RedDBRuntime, sql: &str) -> reddb_server::runtime::RuntimeQueryResult {
    runtime
        .execute_query(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
}

fn assert_temperature_capability_in_both_superblocks(path: &std::path::Path) {
    let bytes = std::fs::read(path).expect("read persistent database");
    for copy_index in 0..PAGED_SUPERBLOCK_SLOT_COUNT {
        let start = copy_index * PAGED_SUPERBLOCK_SLOT_SIZE;
        let slot = &bytes[start..start + PAGED_SUPERBLOCK_SLOT_SIZE];
        assert!(
            paged_superblock_slot_generation(slot, copy_index).is_some(),
            "superblock copy {copy_index} must be valid"
        );
        let header = decode_database_header(slot).expect("decode superblock header");
        assert_ne!(
            header.required_capabilities & FILE_CAPABILITY_TEMPERATURE,
            0,
            "superblock copy {copy_index} must require the temperature codec"
        );
    }
}

fn native_options(path: &std::path::Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
        .expect("native paged profile")
}

#[test]
fn temperature_schema_fences_format_before_values_and_survives_restart() {
    let directory = tempfile::tempdir().expect("private database directory");
    let path = directory.path().join("temperature.rdb");

    {
        let runtime =
            RedDBRuntime::with_options(native_options(&path)).expect("create persistent runtime");
        execute(
            &runtime,
            "CREATE TABLE readings (id INTEGER PRIMARY KEY, value TEMPERATURE NOT NULL)",
        );

        runtime.checkpoint().expect("checkpoint temperature schema");
    }

    // Publishing the schema is already a promise that future rows may use the
    // codec, so the capability must precede the first dependent value.
    assert_temperature_capability_in_both_superblocks(&path);

    {
        let runtime = RedDBRuntime::with_options(native_options(&path)).expect("reopen schema");
        execute(
            &runtime,
            "INSERT INTO readings (id, value) \
             VALUES (1, CAST('20 C' AS TEMPERATURE)), \
                    (2, CAST('68 F' AS TEMPERATURE)), \
                    (3, CAST('293.15 K' AS TEMPERATURE))",
        );
        runtime.checkpoint().expect("checkpoint temperature rows");
    }

    assert_temperature_capability_in_both_superblocks(&path);

    let runtime = RedDBRuntime::with_options(native_options(&path)).expect("reopen database");
    let cache_metrics = runtime.result_cache_metrics();
    let result = execute(&runtime, "SELECT value FROM readings ORDER BY id");
    let expected = [
        Temperature::new(20, 0, TemperatureUnit::Celsius).expect("celsius"),
        Temperature::new(68, 0, TemperatureUnit::Fahrenheit).expect("fahrenheit"),
        Temperature::new(29_315, 2, TemperatureUnit::Kelvin).expect("kelvin"),
    ];
    assert_eq!(result.result.records.len(), expected.len());
    for (record, expected) in result.result.records.iter().zip(expected) {
        assert_eq!(record.get("value"), Some(&Value::Temperature(expected)));
    }
    execute(&runtime, "SELECT value FROM readings ORDER BY id");
    assert_eq!(
        runtime.result_cache_metrics().0 - cache_metrics.0,
        1,
        "result-cache format must fence without downgrading the capability file"
    );

    let equivalent = execute(
        &runtime,
        "SELECT value = CAST('293.15 K' AS TEMPERATURE) AS equivalent \
         FROM readings ORDER BY id",
    );
    assert_eq!(equivalent.result.records.len(), 3);
    assert!(equivalent
        .result
        .records
        .iter()
        .all(|record| record.get("equivalent") == Some(&Value::Boolean(true))));

    runtime
        .checkpoint()
        .expect("checkpoint result-cache state before compaction");
    drop(runtime);
    let compacted = directory.path().join("compacted");
    compact_paged(&path, &compacted).expect("compact temperature database");
    let compacted_path = compacted.join("temperature.rdb");
    assert_temperature_capability_in_both_superblocks(&compacted_path);

    let compacted_runtime = RedDBRuntime::with_options(native_options(&compacted_path))
        .expect("open compacted database");
    let compacted_rows = execute(&compacted_runtime, "SELECT value FROM readings ORDER BY id");
    assert_eq!(compacted_rows.result.records.len(), expected.len());
    for (record, expected) in compacted_rows.result.records.iter().zip(expected) {
        assert_eq!(record.get("value"), Some(&Value::Temperature(expected)));
    }
}
