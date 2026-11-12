use reddb_mcu::{
    file::{
        decode_database_header, paged_superblock_slot_generation, OpenMode, StdVfs, Vfs,
        FILE_CAPABILITY_TEMPERATURE, PAGED_SUPERBLOCK_SLOT_COUNT, PAGED_SUPERBLOCK_SLOT_SIZE,
    },
    types::{Temperature, TemperatureUnit, Value},
    Database, PICO_2,
};

fn assert_temperature_capability(path: &std::path::Path) {
    let bytes = std::fs::read(path).expect("read MCU database");
    for copy_index in 0..PAGED_SUPERBLOCK_SLOT_COUNT {
        let start = copy_index * PAGED_SUPERBLOCK_SLOT_SIZE;
        let slot = &bytes[start..start + PAGED_SUPERBLOCK_SLOT_SIZE];
        assert!(
            paged_superblock_slot_generation(slot, copy_index).is_some(),
            "superblock copy {copy_index} must be valid"
        );
        let header = decode_database_header(slot).expect("decode MCU superblock");
        assert_ne!(
            header.required_capabilities & FILE_CAPABILITY_TEMPERATURE,
            0,
            "superblock copy {copy_index} must require the temperature codec"
        );
    }
}

#[test]
fn package_uses_canonical_sql_and_rdb() {
    let directory = tempfile::tempdir().expect("temporary package database");
    let path = directory.path().join("mcu.rdb");
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("create file");
    let mut database = Database::create(file).expect("create paged database");
    assert_eq!(database.cache_pages(), PICO_2.cache_pages);
    database
        .query(
            "CREATE TABLE samples (observed BIGINT NOT NULL, value TEMPERATURE NOT NULL)",
            1_000,
        )
        .expect("canonical DDL");
    database
        .query(
            "INSERT INTO samples VALUES \
             (1, CAST('20 C' AS TEMPERATURE)), \
             (2, CAST('68 F' AS TEMPERATURE)), \
             (3, CAST('293.15 K' AS TEMPERATURE))",
            2_000,
        )
        .expect("canonical INSERT");
    drop(database.into_inner());
    assert_temperature_capability(&path);

    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("reopen file");
    let database = Database::open(file).expect("recover canonical .rdb");
    let mut rows = Vec::new();
    let summary = database
        .query_stream("SELECT observed, value FROM samples", &[], &mut |row| {
            rows.push(row.to_vec());
            Ok(true)
        })
        .expect("canonical streaming SELECT");
    assert_eq!(summary.rows, 3);
    assert_eq!(
        rows,
        [
            vec![
                Value::BigInt(1),
                Value::Temperature(
                    Temperature::new(20, 0, TemperatureUnit::Celsius).expect("celsius")
                )
            ],
            vec![
                Value::BigInt(2),
                Value::Temperature(
                    Temperature::new(68, 0, TemperatureUnit::Fahrenheit).expect("fahrenheit")
                )
            ],
            vec![
                Value::BigInt(3),
                Value::Temperature(
                    Temperature::new(29_315, 2, TemperatureUnit::Kelvin).expect("kelvin")
                )
            ],
        ]
    );
}
