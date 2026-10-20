use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::{RedDBOptions, RedDBRuntime};

fn fixture() -> RedDBRuntime {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("fixture runtime opens");
    runtime
        .execute_query("CREATE TABLE page_rows (id INTEGER)")
        .expect("fixture table is created");
    for id in 0..10 {
        runtime
            .execute_query(&format!("INSERT INTO page_rows (id) VALUES ({id})"))
            .expect("fixture row is inserted");
    }
    runtime
}

fn identifiers(runtime: &RedDBRuntime, query: &str) -> Vec<String> {
    runtime
        .execute_query(query)
        .expect("valid page query")
        .result
        .records
        .iter()
        .map(|record| {
            record
                .get("id")
                .expect("selected identifier exists")
                .to_string()
        })
        .collect()
}

#[test]
fn limited_windows_preserve_scan_order_and_page_boundaries() {
    let runtime = fixture();
    let all = identifiers(&runtime, "SELECT id FROM page_rows");
    for (offset, limit) in [(0, 0), (0, 3), (5, 2), (9, 5), (12, 4)] {
        assert_eq!(
            identifiers(
                &runtime,
                &format!("SELECT id FROM page_rows LIMIT {limit} OFFSET {offset}"),
            ),
            all.iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn huge_limit_does_not_preallocate_the_requested_capacity() {
    let runtime = fixture();
    assert_eq!(
        identifiers(
            &runtime,
            "SELECT id FROM page_rows LIMIT 9223372036854775807"
        ),
        identifiers(&runtime, "SELECT id FROM page_rows")
    );
}

#[test]
fn huge_offset_does_not_preallocate_skipped_rows() {
    let runtime = fixture();
    assert!(identifiers(
        &runtime,
        "SELECT id FROM page_rows LIMIT 2 OFFSET 9223372036854775807",
    )
    .is_empty());
}

#[test]
fn ordered_windows_apply_offset_after_sorting() {
    let runtime = fixture();
    assert_eq!(
        identifiers(
            &runtime,
            "SELECT id FROM page_rows ORDER BY id DESC LIMIT 3 OFFSET 2"
        ),
        ["7", "6", "5"]
    );
}

#[test]
fn filtered_windows_count_only_matching_rows() {
    let runtime = fixture();
    let all = identifiers(&runtime, "SELECT id FROM page_rows WHERE id >= 5");
    assert_eq!(
        identifiers(
            &runtime,
            "SELECT id FROM page_rows WHERE id >= 5 LIMIT 2 OFFSET 2"
        ),
        all.into_iter().skip(2).take(2).collect::<Vec<_>>()
    );
    assert_eq!(
        identifiers(
            &runtime,
            "SELECT id FROM page_rows WHERE id >= 5 ORDER BY id DESC LIMIT 2 OFFSET 2"
        ),
        ["7", "6"]
    );
}

#[test]
fn finite_windows_preserve_explicit_projection_aliases() {
    let runtime = fixture();
    let expected = identifiers(&runtime, "SELECT id FROM page_rows");
    let result = runtime
        .execute_query("SELECT id AS selected_id FROM page_rows LIMIT 2 OFFSET 3")
        .expect("projected page executes");
    assert_eq!(result.result.records.len(), 2);
    for (record, expected) in result.result.records.iter().zip(expected.iter().skip(3)) {
        assert_eq!(
            record.get("selected_id").expect("alias exists").to_string(),
            *expected
        );
        assert!(
            record.get("id").is_none(),
            "unselected source column leaked"
        );
    }
}

#[test]
fn snapshot_windows_skip_only_visible_versions() {
    let runtime = fixture();
    let execute = |connection, query: &str| {
        set_current_connection_id(connection);
        let result = runtime.execute_query(query);
        clear_current_connection_id();
        result.expect("valid transactional fixture statement")
    };
    execute(10, "BEGIN ISOLATION LEVEL SNAPSHOT");
    assert_eq!(
        execute(10, "SELECT id FROM page_rows").result.records.len(),
        10
    );
    execute(11, "UPDATE page_rows SET id = id + 100");
    execute(11, "INSERT INTO page_rows (id) VALUES (999)");
    // A write can move physical versions in scan order even when the snapshot
    // retains the old values. Compare windows with the same post-write scan.
    let before = execute(10, "SELECT id FROM page_rows").result.records;
    assert_eq!(before.len(), 10);
    for record in &before {
        let id = record.get("id").expect("visible id exists").to_string();
        assert!(id.parse::<u64>().expect("integer id") < 10);
    }
    let page = execute(10, "SELECT id FROM page_rows LIMIT 4 OFFSET 3")
        .result
        .records;
    assert_eq!(page.len(), 4);
    for (record, expected) in page.iter().zip(before.iter().skip(3)) {
        assert_eq!(record.get("id"), expected.get("id"));
    }
    execute(10, "ROLLBACK");
    execute(11, "BEGIN");
    execute(11, "INSERT INTO page_rows (id) VALUES (777)");
    execute(11, "ROLLBACK");
    let all = execute(12, "SELECT id FROM page_rows").result.records;
    let page = execute(12, "SELECT id FROM page_rows LIMIT 4 OFFSET 7")
        .result
        .records;
    assert_eq!(page.len(), 4);
    for (record, expected) in page.iter().zip(all.iter().skip(7)) {
        assert_eq!(record.get("id"), expected.get("id"));
        assert_ne!(record.get("id").expect("id exists").to_string(), "777");
    }
}
