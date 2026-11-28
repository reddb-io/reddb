use reddb_file::{
    decode_published_database_header, FILE_CAPABILITY_CRON, PAGED_SUPERBLOCK_SLOT_SIZE,
};
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use reddb_types::Value;

fn execute(runtime: &RedDBRuntime, sql: &str) -> reddb_server::runtime::RuntimeQueryResult {
    runtime
        .execute_query(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
}

fn timestamp(iso: &str) -> Value {
    reddb_types::coerce::coerce(iso, reddb_types::DataType::TimestampMs, None)
        .expect("test timestamp")
}

#[test]
fn occurrences_expand_filtered_rules_before_order_offset_and_limit() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("memory database");
    execute(&runtime, "CREATE TABLE bookings (id INTEGER PRIMARY KEY, room INTEGER, schedule CRON('America/New_York'))");
    execute(&runtime, "INSERT INTO bookings (id, room, schedule) VALUES (1, 7, '30 1 * * *'), (2, 7, '45 1 * * *'), (3, 8, '* * * * *'), (4, 7, NULL)");
    execute(&runtime, "CREATE INDEX by_room ON bookings (room)");
    let window = [
        timestamp("2026-11-01T05:30:00Z"),
        timestamp("2026-11-01T06:45:00Z"),
    ];
    let sql = "SELECT id, cron_occurrences(schedule, $1, $2) AS at_time FROM bookings WHERE room = 7 ORDER BY at_time OFFSET 1";
    let result = runtime
        .execute_query_with_params(sql, &window)
        .expect("expand the repeated hour")
        .result;
    assert_eq!(result.columns, ["id", "at_time"]);
    let rows = result.records;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].get("id"), Some(&Value::Integer(2)));
    assert_eq!(
        rows[0].get("at_time"),
        Some(&timestamp("2026-11-01T05:45:00Z"))
    );
    assert_eq!(rows[1].get("id"), Some(&Value::Integer(1)));
    assert_eq!(
        rows[1].get("at_time"),
        Some(&timestamp("2026-11-01T06:30:00Z"))
    );

    let limited = runtime
        .execute_query_with_params(
            "SELECT cron_occurrences(CRON('* * * * *'), $1, $2) AS t LIMIT 1 OFFSET 2",
            &window,
        )
        .expect("source-free expansion")
        .result
        .records;
    assert_eq!(limited.len(), 1);
    assert_eq!(
        limited[0].get("t"),
        Some(&timestamp("2026-11-01T05:32:00Z"))
    );
    let sql = "SELECT cron_occurrences(schedule, $1, $2) AS t FROM bookings WHERE id = 1";
    assert!(runtime
        .execute_query_with_params(sql, &[window[1].clone(), window[0].clone()])
        .is_err());
    assert!(runtime
        .execute_query_with_params(sql, &[window[0].clone(), window[0].clone()])
        .expect("empty window")
        .result
        .records
        .is_empty());
    execute(
        &runtime,
        "SET CONFIG stream.executor.max_materialized_rows = 2",
    );
    assert!(runtime.execute_query_with_params("SELECT cron_occurrences(schedule, $1, $2) AS t FROM bookings WHERE room = 7 ORDER BY t", &window).is_err());
}

#[test]
fn cron_type_uses_column_default_and_filters_prepared_windows_after_reopen() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("cron.rdb");
    let open = || {
        let options = RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
            .expect("native paged profile");
        RedDBRuntime::with_options(options).expect("open database")
    };

    {
        let runtime = open();
        execute(
            &runtime,
            "CREATE TABLE jobs (id INTEGER PRIMARY KEY, schedule CRON('America/Sao_Paulo'))",
        );
        runtime.checkpoint().expect("publish the schema");
    }
    let data = std::fs::read(&path).expect("read database header");
    for slot in data.chunks_exact(PAGED_SUPERBLOCK_SLOT_SIZE).take(2) {
        let header = decode_published_database_header(slot).expect("decode superblock");
        assert_ne!(header.required_capabilities & FILE_CAPABILITY_CRON, 0);
    }

    {
        let runtime = open();
        execute(
            &runtime,
            "INSERT INTO jobs (id, schedule) VALUES (1, '0 9 * * 1-5'), \
            (2, CAST(CRON('0 9 * * 1-5', 'America/New_York') AS CRON)), (3, NULL)",
        );
        assert!(runtime
            .execute_query("INSERT INTO jobs (id, schedule) VALUES (4, '60 9 * * *')")
            .is_err());
        assert!(runtime
            .execute_query("UPDATE jobs SET schedule = 'invalid' WHERE id = 1")
            .is_err());
        execute(
            &runtime,
            "UPDATE jobs SET schedule = '15 9 * * 1-5' WHERE id = 1",
        );
        let rows = execute(&runtime, "SELECT id, schedule FROM jobs ORDER BY id")
            .result
            .records;
        assert_eq!(
            rows[0].get("schedule").unwrap().display_string(),
            "15 9 * * 1-5 [America/Sao_Paulo]",
            "{rows:?}"
        );
    }

    let runtime = open();
    let persisted_rows = execute(&runtime, "SELECT id, schedule FROM jobs ORDER BY id")
        .result
        .records;
    assert_eq!(persisted_rows.len(), 3, "{persisted_rows:?}");
    assert_eq!(
        persisted_rows[0].get("schedule").unwrap().display_string(),
        "15 9 * * 1-5 [America/Sao_Paulo]",
        "{persisted_rows:?}"
    );
    let start = timestamp("2026-11-27T12:14:59.999Z");
    let end = timestamp("2026-11-27T12:15:00.001Z");
    let projected = runtime
        .execute_query_with_params(
            "SELECT id, cron_overlaps(schedule, $1, $2) AS in_window FROM jobs ORDER BY id",
            &[start.clone(), end.clone()],
        )
        .expect("project window membership")
        .result
        .records;
    assert_eq!(
        projected[0].get("in_window"),
        Some(&Value::Boolean(true)),
        "{projected:?}"
    );
    assert_eq!(projected[1].get("in_window"), Some(&Value::Boolean(false)));
    assert_eq!(projected[2].get("in_window"), Some(&Value::Null));
    let rows = runtime
        .execute_query_with_params(
            "SELECT id, schedule, cron_next(schedule, $1) AS first_occurrence FROM jobs \
         WHERE cron_overlaps(schedule, $1, $2) ORDER BY id",
            &[start, end],
        )
        .expect("query a time window")
        .result
        .records;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("id"), Some(&Value::Integer(1)));
    assert_eq!(
        rows[0].get("first_occurrence"),
        Some(&timestamp("2026-11-27T12:15:00Z"))
    );
    assert_eq!(
        rows[0].get("schedule").unwrap().display_string(),
        "15 9 * * 1-5 [America/Sao_Paulo]"
    );

    let rows = execute(
        &runtime,
        "SELECT schedule, cron_next(schedule, \
        CAST('2026-11-27T12:00:00Z' AS TIMESTAMPTZ)) AS first_occurrence FROM jobs \
        WHERE id = 2",
    )
    .result
    .records;
    assert_eq!(
        rows[0].get("schedule").unwrap().display_string(),
        "0 9 * * 1-5 [America/New_York]"
    );
    assert_eq!(
        rows[0].get("first_occurrence"),
        Some(&timestamp("2026-11-27T14:00:00Z"))
    );
}

#[test]
fn cron_uses_utc_without_a_column_modifier_and_rejects_invalid_zones_and_windows() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("memory database");
    for sql in [
        "CREATE TABLE bad (schedule CRON('America/Missing'))",
        "CREATE TABLE bad (schedule CRON(3))",
        "CREATE TABLE bad (schedule CRON('UTC', 'UTC'))",
    ] {
        assert!(runtime.execute_query(sql).is_err(), "{sql}");
    }
    execute(
        &runtime,
        "CREATE TABLE jobs (id BIGINT PRIMARY KEY, schedule CRON)",
    );
    execute(
        &runtime,
        "INSERT INTO jobs (id, schedule) VALUES (1, '0 9 * * *')",
    );
    let rows = execute(&runtime, "SELECT schedule FROM jobs")
        .result
        .records;
    assert_eq!(
        rows[0].get("schedule").unwrap().display_string(),
        "0 9 * * * [UTC]"
    );
    let rows = execute(
        &runtime,
        "SELECT cron_next(schedule, NOW()) AS next_run FROM jobs",
    )
    .result
    .records;
    assert!(matches!(
        rows[0].get("next_run"),
        Some(Value::TimestampMs(_))
    ));
    let args = [
        timestamp("2026-11-27T09:00:01Z"),
        timestamp("2026-11-27T09:00:00Z"),
    ];
    assert!(runtime
        .execute_query_with_params(
            "SELECT id FROM jobs WHERE cron_overlaps(schedule, $1, $2)",
            &args,
        )
        .is_err());
}
