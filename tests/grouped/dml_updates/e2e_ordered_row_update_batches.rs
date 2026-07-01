use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn int_field(record: &UnifiedRecord, field: &str) -> i64 {
    match record.get(field) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected {field} integer field, got {other:?} in {record:?}"),
    }
}

fn selected_ids(rt: &RedDBRuntime, table: &str) -> Vec<i64> {
    exec(
        rt,
        &format!("SELECT id FROM {table} WHERE touched = 1 ORDER BY id ASC"),
    )
    .result
    .records
    .iter()
    .map(|record| int_field(record, "id"))
    .collect()
}

fn status_ids(rt: &RedDBRuntime, table: &str, status: &str) -> Vec<i64> {
    exec(
        rt,
        &format!("SELECT id FROM {table} WHERE status = '{status}' ORDER BY id ASC"),
    )
    .result
    .records
    .iter()
    .map(|record| int_field(record, "id"))
    .collect()
}

fn err_string(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
}

fn claim_metric_count(
    snapshot: &reddb::runtime::ClaimTelemetrySnapshot,
    metric: &str,
    collection: &str,
    model: &str,
) -> u64 {
    let rows = match metric {
        "attempts" => &snapshot.attempts,
        "successful" => &snapshot.successful,
        "misses" => &snapshot.misses,
        "skipped_locked" => &snapshot.skipped_locked,
        other => panic!("unknown claim metric {other}"),
    };
    rows.iter()
        .find(|((actual_collection, actual_model), _)| {
            actual_collection == collection && actual_model == model
        })
        .map(|(_, count)| *count)
        .unwrap_or(0)
}

#[test]
fn claim_exact_updates_requested_cardinality_when_available() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE exact_claim_success (id INT, rank INT, status TEXT)",
    );
    // ADR 0063: index-backed claim ordering on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_exact_claim_success_rank ON exact_claim_success (rank)",
    );
    exec(
        &rt,
        "INSERT INTO exact_claim_success (id, rank, status) VALUES \
         (1, 30, 'ready'), (2, 10, 'ready'), (3, 20, 'ready')",
    );

    let updated = exec(
        &rt,
        "UPDATE exact_claim_success SET status = 'claimed' WHERE status = 'ready' \
         CLAIM EXACT 2 ORDER BY rank ASC",
    );

    assert_eq!(updated.affected_rows, 2);
    assert_eq!(
        status_ids(&rt, "exact_claim_success", "claimed"),
        vec![2, 3]
    );
    assert_eq!(status_ids(&rt, "exact_claim_success", "ready"), vec![1]);
}

#[test]
fn claim_exact_miss_reports_zero_and_applies_no_partial_write() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE exact_claim_miss (id INT, rank INT, status TEXT)",
    );
    // ADR 0063: index-backed claim ordering on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_exact_claim_miss_rank ON exact_claim_miss (rank)",
    );
    exec(
        &rt,
        "INSERT INTO exact_claim_miss (id, rank, status) VALUES \
         (1, 10, 'ready'), (2, 20, 'ready')",
    );

    let updated = exec(
        &rt,
        "UPDATE exact_claim_miss SET status = 'claimed' WHERE status = 'ready' \
         CLAIM EXACT 3 ORDER BY rank ASC",
    );

    assert_eq!(updated.affected_rows, 0);
    assert!(status_ids(&rt, "exact_claim_miss", "claimed").is_empty());
    assert_eq!(status_ids(&rt, "exact_claim_miss", "ready"), vec![1, 2]);
}

#[test]
fn claim_inside_transaction_publishes_on_commit() {
    let rt = runtime();
    set_current_connection_id(145201);
    exec(
        &rt,
        "CREATE TABLE tx_claim_commit (id INT, rank INT, status TEXT)",
    );
    // ADR 0063: index-backed claim ordering on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_tx_claim_commit_rank ON tx_claim_commit (rank)",
    );
    exec(
        &rt,
        "INSERT INTO tx_claim_commit (id, rank, status) VALUES \
         (1, 10, 'ready'), (2, 20, 'ready')",
    );

    exec(&rt, "BEGIN");
    let claimed = exec(
        &rt,
        "UPDATE tx_claim_commit SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY rank ASC",
    );
    assert_eq!(claimed.affected_rows, 1);

    set_current_connection_id(145202);
    assert_eq!(status_ids(&rt, "tx_claim_commit", "ready"), vec![1, 2]);

    set_current_connection_id(145201);
    exec(&rt, "COMMIT");

    set_current_connection_id(145202);
    assert_eq!(status_ids(&rt, "tx_claim_commit", "claimed"), vec![1]);
    assert_eq!(status_ids(&rt, "tx_claim_commit", "ready"), vec![2]);
    clear_current_connection_id();
}

#[test]
fn claim_inside_transaction_releases_on_rollback() {
    let rt = runtime();
    set_current_connection_id(145203);
    exec(
        &rt,
        "CREATE TABLE tx_claim_rollback (id INT, rank INT, status TEXT)",
    );
    // ADR 0063: index-backed claim ordering on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_tx_claim_rollback_rank ON tx_claim_rollback (rank)",
    );
    exec(
        &rt,
        "INSERT INTO tx_claim_rollback (id, rank, status) VALUES \
         (1, 10, 'ready'), (2, 20, 'ready')",
    );

    exec(&rt, "BEGIN");
    let claimed = exec(
        &rt,
        "UPDATE tx_claim_rollback SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY rank ASC",
    );
    assert_eq!(claimed.affected_rows, 1);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(145204);
    assert!(status_ids(&rt, "tx_claim_rollback", "claimed").is_empty());
    assert_eq!(status_ids(&rt, "tx_claim_rollback", "ready"), vec![1, 2]);
    let claimed_after_rollback = exec(
        &rt,
        "UPDATE tx_claim_rollback SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY rank ASC",
    );
    assert_eq!(claimed_after_rollback.affected_rows, 1);
    assert_eq!(status_ids(&rt, "tx_claim_rollback", "claimed"), vec![1]);
    clear_current_connection_id();
}

#[test]
fn competing_claim_skips_uncommitted_claim_lock() {
    let rt = runtime();
    set_current_connection_id(145205);
    exec(
        &rt,
        "CREATE TABLE tx_claim_skip (id INT, rank INT, status TEXT)",
    );
    // ADR 0063: index-backed claim ordering on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_tx_claim_skip_rank ON tx_claim_skip (rank)",
    );
    exec(
        &rt,
        "INSERT INTO tx_claim_skip (id, rank, status) VALUES \
         (1, 10, 'ready'), (2, 20, 'ready'), (3, 30, 'ready')",
    );

    exec(&rt, "BEGIN");
    let first = exec(
        &rt,
        "UPDATE tx_claim_skip SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY rank ASC",
    );
    assert_eq!(first.affected_rows, 1);

    set_current_connection_id(145206);
    let second = exec(
        &rt,
        "UPDATE tx_claim_skip SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY rank ASC",
    );
    assert_eq!(second.affected_rows, 1);
    assert_eq!(status_ids(&rt, "tx_claim_skip", "claimed"), vec![2]);

    set_current_connection_id(145205);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(145206);
    assert_eq!(status_ids(&rt, "tx_claim_skip", "ready"), vec![1, 3]);
    clear_current_connection_id();
}

#[test]
fn ordinary_dml_against_claimed_row_keeps_mvcc_conflict_behavior() {
    let rt = runtime();
    set_current_connection_id(145207);
    exec(
        &rt,
        "CREATE TABLE tx_claim_dml (id INT, rank INT, status TEXT, note TEXT)",
    );
    // ADR 0063: index-backed claim ordering on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_tx_claim_dml_rank ON tx_claim_dml (rank)",
    );
    exec(
        &rt,
        "INSERT INTO tx_claim_dml (id, rank, status, note) VALUES \
         (1, 10, 'ready', 'seed')",
    );

    exec(&rt, "BEGIN");
    let claimed = exec(
        &rt,
        "UPDATE tx_claim_dml SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY rank ASC",
    );
    assert_eq!(claimed.affected_rows, 1);

    set_current_connection_id(145208);
    let ordinary = exec(
        &rt,
        "UPDATE tx_claim_dml SET note = 'ordinary' WHERE id = 1",
    );
    assert_eq!(
        ordinary.affected_rows, 0,
        "ordinary DML keeps the existing MVCC visibility behavior while the claim update is open"
    );

    set_current_connection_id(145207);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(145208);
    assert_eq!(status_ids(&rt, "tx_claim_dml", "ready"), vec![1]);
    clear_current_connection_id();
}

#[test]
fn claim_metrics_increment_for_success_and_miss_with_bounded_labels() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE claim_metric_jobs (id INT, rank INT, status TEXT)",
    );
    // ADR 0063: index-backed claim ordering on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_claim_metric_jobs_rank ON claim_metric_jobs (rank)",
    );
    exec(
        &rt,
        "INSERT INTO claim_metric_jobs (id, rank, status) VALUES \
         (1, 10, 'ready'), (2, 20, 'ready')",
    );

    let claimed = exec(
        &rt,
        "UPDATE claim_metric_jobs SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 2 ORDER BY rank ASC",
    );
    let missed = exec(
        &rt,
        "UPDATE claim_metric_jobs SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY rank ASC",
    );

    assert_eq!(claimed.affected_rows, 2);
    assert_eq!(missed.affected_rows, 0);
    let snapshot = rt.claim_telemetry_snapshot();
    assert_eq!(
        claim_metric_count(&snapshot, "attempts", "claim_metric_jobs", "table"),
        2
    );
    assert_eq!(
        claim_metric_count(&snapshot, "successful", "claim_metric_jobs", "table"),
        2
    );
    assert_eq!(
        claim_metric_count(&snapshot, "misses", "claim_metric_jobs", "table"),
        1
    );
    assert_eq!(
        claim_metric_count(&snapshot, "skipped_locked", "claim_metric_jobs", "table"),
        0
    );
    assert_eq!(
        snapshot.attempts.len(),
        1,
        "collection/model labels stay bounded"
    );
    assert_eq!(
        snapshot
            .attempts
            .iter()
            .map(|((collection, model), _)| (collection.as_str(), model.as_str()))
            .collect::<Vec<_>>(),
        vec![("claim_metric_jobs", "table")]
    );
}

#[test]
fn update_order_by_desc_limit_updates_expected_batch() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE ordered_updates (id INT, rank INT, touched INT)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_updates (id, rank, touched) VALUES \
         (1, 10, 0), (2, 30, 0), (3, 20, 0), (4, 40, 0)",
    );

    let updated = exec(
        &rt,
        "UPDATE ordered_updates SET touched = 1 ORDER BY rank DESC LIMIT 2",
    );

    assert_eq!(updated.affected_rows, 2);
    assert_eq!(selected_ids(&rt, "ordered_updates"), vec![2, 4]);
}

#[test]
fn update_order_by_requires_limit_and_top_level_fields() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE ordered_update_rejections (id INT, rank INT, touched INT)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_update_rejections (id, rank, touched) VALUES (1, 10, 0)",
    );

    let without_limit = err_string(
        &rt,
        "UPDATE ordered_update_rejections SET touched = 1 ORDER BY rank",
    );
    assert!(without_limit.contains("ORDER BY requires LIMIT"));

    let expression = err_string(
        &rt,
        "UPDATE ordered_update_rejections SET touched = 1 ORDER BY rank + 1 LIMIT 1",
    );
    assert!(expression.contains("top-level fields"));

    let nested = err_string(
        &rt,
        "UPDATE ordered_update_rejections SET touched = 1 ORDER BY payload.rank LIMIT 1",
    );
    assert!(nested.contains("top-level fields"));
}

#[test]
fn update_order_by_limit_breaks_ties_by_implicit_rid_asc() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE ordered_update_ties (id INT, rank INT, touched INT)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_update_ties (id, rank, touched) VALUES \
         (30, 7, 0), (10, 7, 0), (20, 7, 0)",
    );

    let updated = exec(
        &rt,
        "UPDATE ordered_update_ties SET touched = 1 ORDER BY rank ASC LIMIT 2",
    );

    assert_eq!(updated.affected_rows, 2);
    assert_eq!(selected_ids(&rt, "ordered_update_ties"), vec![10, 30]);
}
