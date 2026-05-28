//! Issue #748 — hypertable + timeseries operational metadata.
//!
//! Pins the contract the Red UI maintenance + chart panels depend on:
//!
//! 1. `red.hypertable_chunks` exposes per-chunk chunk-shaped columns
//!    (`hypertable, chunk_start_ms, chunk_end_ms, row_count,
//!    min_ts_ms, max_ts_ms, sealed, ttl_override_ms,
//!    effective_ttl_ms, expiry_ms, is_expired, tenant_id`) for each
//!    hypertable registered with the runtime.
//! 2. `red.timeseries` is extended with the four maintenance
//!    indicators (`downsample_policies, continuous_aggregate_count,
//!    continuous_aggregate_names, last_sweep_ms`). Optional features
//!    we don't track yet (per-collection sweep time) surface as
//!    NULL rather than fabricated values — AC #3.
//! 3. `red.timeseries_writes` buckets the hypertable's data rows by
//!    1m / 5m / 10m cohort intervals, and accepts `WHERE collection`
//!    / `WHERE bucket_size_ms` filters. `writes_count` is `NULL`
//!    today (actual write throughput unavailable until WAL telemetry
//!    exists), per the thread-discussion decision on this issue —
//!    event-time row counts must not be labelled as true write rate.

use std::collections::HashSet;

use reddb::runtime::RedDBRuntime;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBOptions;

const HYPERTABLE_CHUNK_COLUMNS: [&str; 12] = [
    "hypertable",
    "chunk_start_ms",
    "chunk_end_ms",
    "row_count",
    "min_ts_ms",
    "max_ts_ms",
    "sealed",
    "ttl_override_ms",
    "effective_ttl_ms",
    "expiry_ms",
    "is_expired",
    "tenant_id",
];

const TIMESERIES_WRITES_COLUMNS: [&str; 5] = [
    "collection",
    "bucket_size_ms",
    "bucket_start_ms",
    "events_count",
    "writes_count",
];

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn select(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn assert_columns(actual: &[String], expected: &[&str]) {
    let actual: HashSet<&str> = actual.iter().map(String::as_str).collect();
    let expected: HashSet<&str> = expected.iter().copied().collect();
    assert_eq!(actual, expected, "column set mismatch");
}

fn uint(row: &UnifiedRecord, col: &str) -> u64 {
    match row.get(col) {
        Some(Value::UnsignedInteger(n)) => *n,
        other => panic!("expected uint at `{col}`, got {other:?}"),
    }
}

fn boolean(row: &UnifiedRecord, col: &str) -> bool {
    match row.get(col) {
        Some(Value::Boolean(v)) => *v,
        other => panic!("expected bool at `{col}`, got {other:?}"),
    }
}

const HOUR_MS: u64 = 3_600_000;
const HOUR_NS: u64 = HOUR_MS * 1_000_000;

#[test]
fn red_hypertable_chunks_exposes_chunk_shaped_columns_and_ranges() {
    let rt = rt();
    exec(
        &rt,
        "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h' TTL '24h'",
    );
    // Allocate three chunks at hours 0, 1, 2 — `route()` updates the
    // registry's chunk metadata exactly as the INSERT path would.
    let db = rt.db();
    let reg = db.hypertables();
    reg.route("metrics", 0).expect("route 0");
    reg.route("metrics", HOUR_NS).expect("route 1");
    reg.route("metrics", 2 * HOUR_NS).expect("route 2");

    let result = select(&rt, "SELECT * FROM red.hypertable_chunks");
    assert_columns(&result.result.columns, &HYPERTABLE_CHUNK_COLUMNS);

    let mut rows: Vec<&UnifiedRecord> = result
        .result
        .records
        .iter()
        .filter(|r| matches!(r.get("hypertable"), Some(Value::Text(t)) if &**t == "metrics"))
        .collect();
    rows.sort_by_key(|r| uint(r, "chunk_start_ms"));
    assert_eq!(rows.len(), 3, "expected 3 chunks, got {}", rows.len());

    assert_eq!(uint(rows[0], "chunk_start_ms"), 0);
    assert_eq!(uint(rows[0], "chunk_end_ms"), HOUR_MS);
    assert_eq!(uint(rows[1], "chunk_start_ms"), HOUR_MS);
    assert_eq!(uint(rows[1], "chunk_end_ms"), 2 * HOUR_MS);
    assert_eq!(uint(rows[2], "chunk_start_ms"), 2 * HOUR_MS);

    for row in &rows {
        assert_eq!(uint(row, "row_count"), 1, "each chunk got one routed ts");
        // Min/max ts on a single-routed chunk equal that ts.
        assert_eq!(uint(row, "min_ts_ms"), uint(row, "chunk_start_ms"));
        assert_eq!(uint(row, "max_ts_ms"), uint(row, "chunk_start_ms"));
        // 24h TTL on the hypertable propagates as the effective TTL.
        assert_eq!(uint(row, "effective_ttl_ms"), 24 * HOUR_MS);
        // No per-chunk override applied at CREATE time.
        assert!(matches!(row.get("ttl_override_ms"), Some(Value::Null)));
        // expiry = max_ts + effective_ttl.
        let expected_expiry = uint(row, "max_ts_ms") + 24 * HOUR_MS;
        assert_eq!(uint(row, "expiry_ms"), expected_expiry);
    }
}

#[test]
fn red_hypertable_chunks_marks_empty_chunks_with_null_bounds() {
    let rt = rt();
    exec(
        &rt,
        "CREATE HYPERTABLE empty_ht TIME_COLUMN ts CHUNK_INTERVAL '1h'",
    );
    // No routes — registry has no chunks at all yet.
    let result = select(
        &rt,
        "SELECT * FROM red.hypertable_chunks WHERE hypertable = 'empty_ht'",
    );
    assert!(
        result.result.records.is_empty(),
        "no chunks routed → no rows: {:?}",
        result.result.records
    );
}

#[test]
fn red_hypertable_chunks_skips_non_hypertable_collections() {
    let rt = rt();
    exec(&rt, "CREATE TIMESERIES legacy RETENTION 1 d");
    exec(&rt, "CREATE TABLE plain_table (id INT)");

    let result = select(&rt, "SELECT * FROM red.hypertable_chunks");
    assert_columns(&result.result.columns, &HYPERTABLE_CHUNK_COLUMNS);
    let names: HashSet<String> = result
        .result
        .records
        .iter()
        .filter_map(|r| match r.get("hypertable") {
            Some(Value::Text(t)) => Some(t.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        !names.contains("legacy"),
        "plain timeseries are not hypertables: {names:?}"
    );
    assert!(
        !names.contains("plain_table"),
        "plain tables are not hypertables: {names:?}"
    );
}

#[test]
fn red_timeseries_surfaces_downsample_policies_and_continuous_aggregates() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TIMESERIES metrics RETENTION 7 d DOWNSAMPLE 1h:5m:avg, 1d:1h:max",
    );
    // Register two continuous aggregates whose source = metrics, plus
    // one whose source is unrelated to prove the per-source filter.
    exec(
        &rt,
        "SELECT CA_REGISTER('one_min_load', 'metrics', '1m', 'avg_load', 'avg', 'load') AS ok",
    );
    exec(
        &rt,
        "SELECT CA_REGISTER('one_hour_load', 'metrics', '1h', 'avg_load', 'avg', 'load') AS ok",
    );
    exec(&rt, "CREATE TIMESERIES other RETENTION 1 d");
    exec(
        &rt,
        "SELECT CA_REGISTER('other_ca', 'other', '5m', 'sum_v', 'sum', 'v') AS ok",
    );

    let result = select(&rt, "SELECT * FROM red.timeseries WHERE name = 'metrics'");
    let row = result
        .result
        .records
        .iter()
        .find(|r| matches!(r.get("name"), Some(Value::Text(t)) if &**t == "metrics"))
        .expect("metrics row");

    // Two declared downsample policies, sorted + comma-joined.
    let policies = match row.get("downsample_policies") {
        Some(Value::Text(t)) => t.to_string(),
        other => panic!("expected text downsample_policies, got {other:?}"),
    };
    assert_eq!(policies, "1d:1h:max,1h:5m:avg");

    // Two CAs target metrics; the third (other_ca) must be excluded.
    assert_eq!(uint(row, "continuous_aggregate_count"), 2);
    let names = match row.get("continuous_aggregate_names") {
        Some(Value::Text(t)) => t.to_string(),
        other => panic!("expected text continuous_aggregate_names, got {other:?}"),
    };
    assert_eq!(names, "one_hour_load,one_min_load");

    // Per-collection sweep time isn't tracked yet — explicit NULL,
    // not a fabricated zero / now() (AC #3).
    assert!(matches!(row.get("last_sweep_ms"), Some(Value::Null)));
}

#[test]
fn red_timeseries_reports_null_indicators_when_features_absent() {
    let rt = rt();
    exec(&rt, "CREATE TIMESERIES bare RETENTION 1 d");

    let result = select(&rt, "SELECT * FROM red.timeseries WHERE name = 'bare'");
    let row = result
        .result
        .records
        .iter()
        .find(|r| matches!(r.get("name"), Some(Value::Text(t)) if &**t == "bare"))
        .expect("bare row");

    assert!(matches!(row.get("downsample_policies"), Some(Value::Null)));
    assert_eq!(uint(row, "continuous_aggregate_count"), 0);
    assert!(matches!(
        row.get("continuous_aggregate_names"),
        Some(Value::Null)
    ));
    assert!(matches!(row.get("last_sweep_ms"), Some(Value::Null)));
}

#[test]
fn red_timeseries_writes_buckets_rows_at_canonical_cohort_sizes() {
    let rt = rt();
    exec(
        &rt,
        "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1h'",
    );
    // Insert four rows at known times.
    //   ts (ns)            ts (ms)   1m bucket   5m bucket   10m bucket
    //     0                0          0           0           0
    //     30_000_000_000   30_000     0           0           0
    //     90_000_000_000   90_000     60_000      60_000      60_000
    //    400_000_000_000   400_000    360_000     300_000     360_000
    // (ms=30_000 falls in [0, 60_000) for 1m, in [0, 300_000) for 5m
    //  and in [0, 600_000) for 10m; ms=90_000 falls in [60_000,
    //  120_000) for 1m / [0, 300_000) for 5m / [0, 600_000) for 10m;
    //  ms=400_000 falls in [360_000, 420_000) for 1m, [300_000,
    //  600_000) for 5m, and [0, 600_000) for 10m.)
    for ts_ns in [0u64, 30_000_000_000, 90_000_000_000, 400_000_000_000] {
        exec(
            &rt,
            &format!("INSERT INTO metrics (ts, value) VALUES ({ts_ns}, 1)"),
        );
    }

    // Without filters: rows for all three cohorts, every non-empty
    // bucket. Pin the contract by counting per-cohort row totals.
    let result = select(&rt, "SELECT * FROM red.timeseries_writes");
    assert_columns(&result.result.columns, &TIMESERIES_WRITES_COLUMNS);

    let mut totals_by_cohort: std::collections::HashMap<u64, u64> =
        std::collections::HashMap::new();
    for r in &result.result.records {
        if !matches!(r.get("collection"), Some(Value::Text(t)) if &**t == "metrics") {
            continue;
        }
        let size = uint(r, "bucket_size_ms");
        let count = uint(r, "events_count");
        *totals_by_cohort.entry(size).or_insert(0) += count;
        // writes_count must stay NULL (unavailable) — per thread-discussion.
        assert!(
            matches!(r.get("writes_count"), Some(Value::Null)),
            "writes_count must be NULL today, got {:?}",
            r.get("writes_count"),
        );
    }
    // Every cohort observes all four inserted rows.
    assert_eq!(totals_by_cohort.get(&60_000).copied(), Some(4));
    assert_eq!(totals_by_cohort.get(&300_000).copied(), Some(4));
    assert_eq!(totals_by_cohort.get(&600_000).copied(), Some(4));

    // With WHERE bucket_size_ms = 60_000 we expect 1m buckets only.
    let one_min = select(
        &rt,
        "SELECT * FROM red.timeseries_writes WHERE bucket_size_ms = 60000",
    );
    let bucket_sizes: HashSet<u64> = one_min
        .result
        .records
        .iter()
        .map(|r| uint(r, "bucket_size_ms"))
        .collect();
    assert_eq!(
        bucket_sizes,
        HashSet::from([60_000]),
        "WHERE bucket_size_ms = 60000 narrows to that cohort: {bucket_sizes:?}"
    );

    // The 1m cohort should produce three distinct buckets: 0, 60_000,
    // 360_000 (ms=0 and ms=30_000 collapse into bucket 0).
    let mut one_min_buckets: Vec<(u64, u64)> = one_min
        .result
        .records
        .iter()
        .map(|r| (uint(r, "bucket_start_ms"), uint(r, "events_count")))
        .collect();
    one_min_buckets.sort();
    assert_eq!(
        one_min_buckets,
        vec![(0, 2), (60_000, 1), (360_000, 1)],
        "1m bucket distribution"
    );
}

#[test]
fn red_timeseries_writes_collection_filter_narrows_to_one_hypertable() {
    let rt = rt();
    exec(
        &rt,
        "CREATE HYPERTABLE alpha TIME_COLUMN ts CHUNK_INTERVAL '1h'",
    );
    exec(
        &rt,
        "CREATE HYPERTABLE beta TIME_COLUMN ts CHUNK_INTERVAL '1h'",
    );
    exec(&rt, "INSERT INTO alpha (ts, value) VALUES (0, 1)");
    exec(&rt, "INSERT INTO beta (ts, value) VALUES (0, 2)");

    let result = select(
        &rt,
        "SELECT * FROM red.timeseries_writes WHERE collection = 'alpha'",
    );
    let names: HashSet<String> = result
        .result
        .records
        .iter()
        .filter_map(|r| match r.get("collection") {
            Some(Value::Text(t)) => Some(t.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        HashSet::from(["alpha".to_string()]),
        "WHERE collection = 'alpha' must drop beta: {names:?}"
    );
}

#[test]
fn red_timeseries_writes_skips_non_hypertable_timeseries() {
    let rt = rt();
    // Plain CREATE TIMESERIES does not declare a time column, so the
    // bucketed-writes surface has nothing to bucket by — it must not
    // emit rows for these collections.
    exec(&rt, "CREATE TIMESERIES legacy RETENTION 1 d");

    let result = select(&rt, "SELECT * FROM red.timeseries_writes");
    assert!(
        result.result.records.is_empty(),
        "plain timeseries has no time column to bucket: {:?}",
        result.result.records
    );

    // Sanity: also no rows when filtered to that specific collection.
    let filtered = select(
        &rt,
        "SELECT * FROM red.timeseries_writes WHERE collection = 'legacy'",
    );
    assert!(filtered.result.records.is_empty());
}

#[test]
fn red_hypertable_chunks_respects_tenant_scope() {
    let rt = rt();
    exec(&rt, "SET TENANT 'acme'");
    exec(
        &rt,
        "CREATE HYPERTABLE acme_ht TIME_COLUMN ts CHUNK_INTERVAL '1h'",
    );
    rt.db().hypertables().route("acme_ht", 0).unwrap();

    exec(&rt, "SET TENANT 'globex'");
    exec(
        &rt,
        "CREATE HYPERTABLE globex_ht TIME_COLUMN ts CHUNK_INTERVAL '1h'",
    );
    rt.db().hypertables().route("globex_ht", 0).unwrap();

    let visible: HashSet<String> = select(&rt, "SELECT hypertable FROM red.hypertable_chunks")
        .result
        .records
        .iter()
        .filter_map(|r| match r.get("hypertable") {
            Some(Value::Text(t)) => Some(t.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        visible.contains("globex_ht"),
        "globex sees its own chunks: {visible:?}"
    );
    assert!(
        !visible.contains("acme_ht"),
        "globex must not see acme chunks: {visible:?}"
    );

    exec(&rt, "SET TENANT NULL");
    let admin: HashSet<String> = select(&rt, "SELECT hypertable FROM red.hypertable_chunks")
        .result
        .records
        .iter()
        .filter_map(|r| match r.get("hypertable") {
            Some(Value::Text(t)) => Some(t.to_string()),
            _ => None,
        })
        .collect();
    assert!(admin.contains("acme_ht"));
    assert!(admin.contains("globex_ht"));
}

#[test]
fn red_hypertable_chunks_marks_expired_chunks_against_now() {
    let rt = rt();
    // 1ns TTL means every routed chunk is immediately expired against
    // current wall-clock time — pins that `is_expired` is computed
    // dynamically rather than baked at route() time.
    exec(
        &rt,
        "CREATE HYPERTABLE shortlived TIME_COLUMN ts CHUNK_INTERVAL '1h' TTL '1ms'",
    );
    rt.db().hypertables().route("shortlived", 0).unwrap();

    let result = select(
        &rt,
        "SELECT * FROM red.hypertable_chunks WHERE hypertable = 'shortlived'",
    );
    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    assert!(
        boolean(row, "is_expired"),
        "chunk with 1ms TTL routed at ts=0 must be expired against now()"
    );
    assert_eq!(uint(row, "effective_ttl_ms"), 1);
}
