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
    let first = runtime.execute_query_with_params("SELECT cron_occurrences(schedule, $1, $2) AS t FROM bookings WHERE room = 8 ORDER BY t LIMIT 1", &window)
        .expect("only the first occurrence of a dense rule is needed");
    assert_eq!(first.result.records[0].get("t"), Some(&window[0]));
    let outer = runtime.execute_query_with_params(
        "SELECT t FROM (SELECT cron_occurrences(schedule, $1, $2) AS t FROM bookings WHERE id = 1) AS agenda ORDER BY t DESC LIMIT 1",
        &window,
    ).expect("query the expanded rows").result.records;
    assert_eq!(outer.len(), 1);
    assert_eq!(outer[0].get("t"), Some(&timestamp("2026-11-01T06:30:00Z")));
}

#[test]
fn first_conflict_includes_carry_in_and_respects_visibility_and_updates() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("memory database");
    execute(&rt, "CREATE TABLE bookings (id INTEGER PRIMARY KEY, room INTEGER, schedule CRON, duration_ms INTEGER)");
    execute(&rt, "INSERT INTO bookings (id, room, schedule, duration_ms) VALUES (1,7,'30 23 * * *',1800000),(2,7,'45 23 * * *',3600000),(3,8,'0 23 * * *',7200000),(4,7,'30 0 * * *',900000)");
    execute(
        &rt,
        "CREATE POLICY visible ON bookings FOR SELECT USING (room = 7)",
    );
    execute(&rt, "ALTER TABLE bookings ENABLE ROW LEVEL SECURITY");
    let window = [
        timestamp("2026-11-27T00:00:00Z"),
        timestamp("2026-11-27T00:30:00Z"),
    ];
    let sql = "SELECT id, cron_occurrences(schedule, $1 - duration_ms + 1, $2) AS begins_at FROM bookings ORDER BY begins_at, id LIMIT 1";
    execute(
        &rt,
        "CREATE INDEX calendar ON bookings (schedule) USING CRON",
    );
    let indexed_sql = "SELECT id, cron_occurrences(schedule, $1 - duration_ms + 1, $2) AS begins_at FROM bookings WHERE cron_overlaps(schedule, $3, $2) ORDER BY begins_at, id LIMIT 1";
    let bounds = [
        window[0].clone(),
        window[1].clone(),
        timestamp("2026-11-26T22:00:00.001Z"),
    ];
    let indexed = rt
        .execute_query_with_params(indexed_sql, &bounds)
        .expect("indexed conflict")
        .result
        .records;
    assert_eq!(indexed.len(), 1);
    assert_eq!(indexed[0].get("id"), Some(&Value::Integer(2)));
    let rows = rt
        .execute_query_with_params(sql, &window)
        .expect("occupied across midnight")
        .result
        .records;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].get("id"), Some(&Value::Integer(2)));
    assert_eq!(
        rows[0].get("begins_at"),
        Some(&timestamp("2026-11-26T23:45:00Z"))
    );
    execute(
        &rt,
        "CREATE POLICY editable ON bookings FOR UPDATE USING (room = 7)",
    );
    execute(
        &rt,
        "UPDATE bookings SET schedule = '0 3 * * *' WHERE id = 2",
    );
    assert!(rt
        .execute_query_with_params(sql, &window)
        .expect("free after rescheduling")
        .result
        .records
        .is_empty());
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

#[test]
fn cron_index_matches_the_scan_after_reopen_and_mutations() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("indexed-cron.rdb");
    let open = || {
        RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
                .expect("paged profile"),
        )
        .expect("open database")
    };
    let runtime = open();
    execute(
        &runtime,
        "CREATE TABLE jobs (id INTEGER PRIMARY KEY, schedule CRON('America/New_York'))",
    );
    execute(&runtime, "INSERT INTO jobs (id, schedule) VALUES (1, '30 1 * * *'), (2, '45 1 * * *'), (3, NULL), (4, '30 5 * * * [UTC]'), (5, '30 1 5 * SUN')");
    let select = |rt: &RedDBRuntime, start: &str, end: &str| {
        rt.execute_query_with_params(
            "SELECT id FROM jobs WHERE cron_overlaps(schedule, $1, $2) ORDER BY id",
            &[timestamp(start), timestamp(end)],
        )
        .expect("query window")
        .result
        .records
        .into_iter()
        .map(|r| r.get("id").cloned().expect("id"))
        .collect::<Vec<_>>()
    };
    let windows = [
        ("2026-11-01T05:30:00Z", "2026-11-01T05:30:00.001Z"),
        ("2026-11-01T06:30:00Z", "2026-11-01T06:45:00Z"),
        ("2026-11-01T05:45:00.001Z", "2026-11-01T06:30:00Z"),
        ("2026-03-08T06:00:00Z", "2026-03-09T04:00:00Z"),
        ("2026-10-01T00:00:00Z", "2026-12-01T00:00:00Z"),
    ];
    let expected = windows.map(|(a, b)| select(&runtime, a, b));
    assert_eq!(
        expected[0],
        [Value::Integer(1), Value::Integer(4), Value::Integer(5)]
    );
    assert_eq!(expected[1], [Value::Integer(1), Value::Integer(5)]);
    assert!(expected[2].is_empty());
    runtime.checkpoint().expect("publish rows before backfill");
    for ddl in [
        "CREATE UNIQUE INDEX bad ON jobs (schedule) USING CRON",
        "CREATE INDEX bad ON jobs (id) USING CRON",
        "CREATE INDEX bad ON jobs (schedule, id) USING CRON",
    ] {
        assert!(runtime.execute_query(ddl).is_err(), "{ddl}");
    }
    execute(
        &runtime,
        "CREATE INDEX schedule_calendar ON jobs (schedule) USING CRON",
    );
    for ((a, b), want) in windows.iter().zip(&expected) {
        assert_eq!(&select(&runtime, a, b), want);
    }
    runtime.checkpoint().expect("publish index descriptor");
    drop(runtime);
    let runtime = open();
    let index = runtime
        .index_store_ref()
        .find_index_for_column("jobs", "schedule")
        .expect("restored index");
    assert_eq!(index.name, "schedule_calendar");
    assert_eq!(runtime.index_store_ref().entries_indexed(&index), Some(4));
    for ((a, b), want) in windows.iter().zip(&expected) {
        assert_eq!(&select(&runtime, a, b), want);
    }
    execute(
        &runtime,
        "UPDATE jobs SET schedule = '0 0 * * *' WHERE id = 1",
    );
    execute(&runtime, "DELETE FROM jobs WHERE id = 4");
    execute(
        &runtime,
        "INSERT INTO jobs (id, schedule) VALUES (6, '30 5 * * * [UTC]')",
    );
    let (a, b) = windows[0];
    let after = select(&runtime, a, b);
    assert_eq!(after, [Value::Integer(5), Value::Integer(6)]);
    execute(&runtime, "BEGIN");
    execute(
        &runtime,
        "UPDATE jobs SET schedule = '5 5 * * *' WHERE id = 6",
    );
    assert_eq!(select(&runtime, a, b), [Value::Integer(5)]);
    execute(&runtime, "ROLLBACK");
    assert_eq!(select(&runtime, a, b), after);
    execute(&runtime, "DROP INDEX schedule_calendar ON jobs");
    assert_eq!(select(&runtime, a, b), after);
}

#[test]
fn cron_index_prunes_selective_minutes_and_rechecks_cross_hour_windows() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    execute(
        &runtime,
        "CREATE TABLE clock_slots (id INTEGER, schedule CRON)",
    );
    execute(
        &runtime,
        "CREATE INDEX by_slot ON clock_slots (schedule) USING CRON",
    );
    let values = (0..240)
        .map(|id| format!("({id}, '{} {} * * *')", id % 60, id / 60))
        .collect::<Vec<_>>()
        .join(", ");
    execute(
        &runtime,
        &format!("INSERT INTO clock_slots (id, schedule) VALUES {values}"),
    );
    let rid = execute(&runtime, "SELECT rid FROM clock_slots WHERE id = 0")
        .result
        .records[0]
        .get("rid")
        .cloned()
        .expect("physical row");
    let bounded = runtime
        .execute_query_with_params(
            "SELECT id FROM clock_slots WHERE rid = $3 AND cron_overlaps(schedule, $1, $2)",
            &[
                timestamp("2026-11-01T01:30:00Z"),
                timestamp("2026-11-01T01:31:00Z"),
                rid,
            ],
        )
        .expect("row ID with calendar residual");
    assert!(bounded.result.records.is_empty());
    let sql = "SELECT id FROM clock_slots WHERE cron_overlaps(schedule, $1, $2) ORDER BY id";
    for (a, b, expected) in [
        ("2026-11-01T01:30:00Z", "2026-11-01T01:31:00Z", vec![90]),
        (
            "2026-11-01T01:58:00Z",
            "2026-11-01T02:02:00Z",
            vec![118, 119, 120, 121],
        ),
    ] {
        let result = runtime
            .execute_query_with_params(sql, &[timestamp(a), timestamp(b)])
            .expect("lookup");
        let ids: Vec<_> = result
            .result
            .records
            .iter()
            .map(|r| r.get("id").cloned())
            .collect();
        assert_eq!(
            ids,
            expected
                .into_iter()
                .map(|i| Some(Value::Integer(i)))
                .collect::<Vec<_>>()
        );
    }
}
