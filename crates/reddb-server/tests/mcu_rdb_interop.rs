//! The MCU package writes the canonical native file, not an MCU-only dialect.
use reddb_engine::query::local_database::PagedLocalDatabase;
use reddb_file::{OpenMode, StdVfs, Vfs};
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[test]
fn canonical_server_reads_mcu_paged_rdb() {
    let directory = tempfile::tempdir().expect("temporary interoperability database");
    let path = directory.path().join("mcu.rdb");
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("create MCU file");
    let mut database = PagedLocalDatabase::create(file).expect("create MCU database");
    database
        .query(
            "CREATE TABLE measurements (recorded_at BIGINT PRIMARY KEY, temperature TEMPERATURE NOT NULL)",
            1_000,
        )
        .expect("create canonical table");
    database
        .query(
            "INSERT INTO measurements VALUES \
             (1, CAST('20 C' AS TEMPERATURE)), \
             (2, CAST('68 F' AS TEMPERATURE)), \
             (3, CAST('293.15 K' AS TEMPERATURE))",
            2_000,
        )
        .expect("insert canonical rows");
    drop(database.into_inner());

    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
        .expect("native paged profile")
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(false)
        .with_read_only(true);
    let runtime = RedDBRuntime::with_options(options).expect("canonical server opens MCU .rdb");
    let result = runtime
        .execute_query("SELECT recorded_at, temperature FROM measurements ORDER BY recorded_at")
        .expect("canonical server queries MCU rows");
    let rows: Vec<_> = result
        .result
        .records
        .iter()
        .map(|row| {
            (
                row.get("recorded_at")
                    .expect("recorded_at")
                    .display_string(),
                row.get("temperature")
                    .expect("temperature")
                    .display_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("1".into(), "20 C".into()),
            ("2".into(), "68 F".into()),
            ("3".into(), "293.15 K".into()),
        ]
    );
}
