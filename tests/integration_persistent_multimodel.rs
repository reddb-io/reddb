mod support;

use support::{
    assert_native_consistency, assert_shared_query_behavior, assert_sql_function_queries,
    build_api_fixture, build_sql_fixture, checkpoint_and_reopen, logical_snapshot,
    PersistentDbPath,
};

use reddb::application::{ExecuteQueryInput, QueryUseCases};
use reddb::runtime::RuntimeQueryResult;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_graph_then_table_reopens_and_scans_both_models() {
    let path = PersistentDbPath::new("graph_then_table");
    let rt = path.open_runtime();

    let hansel = insert_graph_node(&rt, "hansel", "Hansel");
    let gretel = insert_graph_node(&rt, "gretel", "Gretel");
    exec(
        &rt,
        &format!(
            "INSERT INTO tales EDGE (label, from, to, evidence) VALUES ('HAS_TRAIT', {hansel}, {gretel}, 'siblings') RETURNING *"
        ),
    );

    exec(
        &rt,
        "CREATE TABLE tale_words (tale_slug TEXT, word TEXT, occurrences INTEGER)",
    );
    exec(
        &rt,
        "INSERT INTO tale_words (tale_slug, word, occurrences) VALUES ('hansel-gretel', 'forest', 3), ('hansel-gretel', 'witch', 2)",
    );

    let rt = checkpoint_and_reopen(&path, rt);

    let words = exec(
        &rt,
        "SELECT word, occurrences FROM tale_words ORDER BY word ASC",
    );
    assert_eq!(words.result.records.len(), 2);
    assert_eq!(text_at(&words, 0, "word"), "forest");
    assert_eq!(uint_at(&words, 0, "occurrences"), 3);
    assert_eq!(text_at(&words, 1, "word"), "witch");
    assert_eq!(uint_at(&words, 1, "occurrences"), 2);

    let path = exec(
        &rt,
        &format!(
            "GRAPH SHORTEST_PATH '{}' TO '{}' ALGORITHM dijkstra",
            hansel, gretel
        ),
    );
    assert_eq!(uint_at(&path, 0, "hop_count"), 1);
}

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_medium_graph_then_table_reopens_without_checksum_or_schema_loss() {
    const NODE_COUNT: usize = 512;
    const TABLE_ROWS: usize = 4096;
    const BATCH_SIZE: usize = 128;

    let path = PersistentDbPath::new("medium_graph_then_table");
    let rt = path.open_runtime();

    let node_ids = insert_graph_nodes(&rt, "tales_medium", NODE_COUNT, BATCH_SIZE);
    insert_graph_chain_edges(&rt, "tales_medium", &node_ids, BATCH_SIZE);

    exec(
        &rt,
        "CREATE TABLE tale_words_medium (tale_slug TEXT, word TEXT, occurrences INTEGER)",
    );
    insert_word_rows(&rt, "tale_words_medium", TABLE_ROWS, BATCH_SIZE);

    let rt = checkpoint_and_reopen(&path, rt);

    let words = exec(
        &rt,
        "SELECT word, occurrences FROM tale_words_medium ORDER BY word ASC",
    );
    assert_eq!(words.result.records.len(), TABLE_ROWS);
    assert_eq!(text_at(&words, 0, "word"), "word_0000");
    assert_eq!(uint_at(&words, 0, "occurrences"), 1);
    assert_eq!(text_at(&words, TABLE_ROWS - 1, "word"), "word_4095");
    assert_eq!(uint_at(&words, TABLE_ROWS - 1, "occurrences"), 6);

    let path = exec(
        &rt,
        &format!(
            "GRAPH SHORTEST_PATH '{}' TO '{}' ALGORITHM dijkstra",
            node_ids[0],
            node_ids[NODE_COUNT - 1]
        ),
    );
    assert_eq!(uint_at(&path, 0, "hop_count"), (NODE_COUNT - 1) as u64);
}

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_sql_fixture_reopens_with_same_logical_snapshot() {
    let path = PersistentDbPath::new("sql_fixture");
    let rt = path.open_runtime();

    build_sql_fixture(&rt);
    let before = logical_snapshot(&rt);

    let rt = checkpoint_and_reopen(&path, rt);
    let after = logical_snapshot(&rt);

    assert_eq!(after, before);
    assert_shared_query_behavior(&rt);
    assert_sql_function_queries(&rt);
}

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_api_fixture_reopens_with_same_logical_snapshot() {
    let path = PersistentDbPath::new("api_fixture");
    let rt = path.open_runtime();

    build_api_fixture(&rt);
    let before = logical_snapshot(&rt);

    let rt = checkpoint_and_reopen(&path, rt);
    let after = logical_snapshot(&rt);

    assert_eq!(after, before);
    assert_shared_query_behavior(&rt);
}

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_sql_and_api_fixtures_match() {
    let sql_path = PersistentDbPath::new("sql_match");
    let api_path = PersistentDbPath::new("api_match");

    let sql_rt = sql_path.open_runtime();
    build_sql_fixture(&sql_rt);
    let sql_rt = checkpoint_and_reopen(&sql_path, sql_rt);

    let api_rt = api_path.open_runtime();
    build_api_fixture(&api_rt);
    let api_rt = checkpoint_and_reopen(&api_path, api_rt);

    assert_eq!(logical_snapshot(&sql_rt), logical_snapshot(&api_rt));
    assert_shared_query_behavior(&sql_rt);
    assert_shared_query_behavior(&api_rt);
}

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_native_metadata_and_catalog_stay_consistent() {
    let path = PersistentDbPath::new("native_consistency");
    let rt = path.open_runtime();

    build_api_fixture(&rt);
    let rt = checkpoint_and_reopen(&path, rt);

    assert_native_consistency(&rt);
    assert_shared_query_behavior(&rt);
}

fn insert_graph_node(rt: &RedDBRuntime, label: &str, name: &str) -> u64 {
    let result = exec(
        rt,
        &format!("INSERT INTO tales NODE (label, name) VALUES ('{label}', '{name}') RETURNING *"),
    );
    uint_at(&result, 0, "red_entity_id")
}

fn insert_graph_nodes(
    rt: &RedDBRuntime,
    collection: &str,
    count: usize,
    batch_size: usize,
) -> Vec<u64> {
    let mut ids = Vec::with_capacity(count);
    for start in (0..count).step_by(batch_size) {
        let end = (start + batch_size).min(count);
        let values = (start..end)
            .map(|i| format!("('node_{i:04}', 'Character', 'Node {i}')"))
            .collect::<Vec<_>>()
            .join(", ");
        let result = exec(
            rt,
            &format!(
                "INSERT INTO {collection} NODE (label, node_type, name) VALUES {values} RETURNING *"
            ),
        );
        assert_eq!(result.result.records.len(), end - start);
        for row in 0..result.result.records.len() {
            ids.push(uint_at(&result, row, "red_entity_id"));
        }
    }
    ids
}

fn insert_graph_chain_edges(
    rt: &RedDBRuntime,
    collection: &str,
    node_ids: &[u64],
    batch_size: usize,
) {
    for start in (0..node_ids.len() - 1).step_by(batch_size) {
        let end = (start + batch_size).min(node_ids.len() - 1);
        let values = (start..end)
            .map(|i| {
                format!(
                    "('NEXT', {}, {}, 1.0, 'chain evidence {i}')",
                    node_ids[i],
                    node_ids[i + 1]
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let result = exec(
            rt,
            &format!(
                "INSERT INTO {collection} EDGE (label, from, to, weight, evidence) VALUES {values} RETURNING *"
            ),
        );
        assert_eq!(result.result.records.len(), end - start);
    }
}

fn insert_word_rows(rt: &RedDBRuntime, table: &str, count: usize, batch_size: usize) {
    for start in (0..count).step_by(batch_size) {
        let end = (start + batch_size).min(count);
        let values = (start..end)
            .map(|i| format!("('tale_{:04}', 'word_{:04}', {})", i % 206, i, (i % 10) + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let result = exec(
            rt,
            &format!("INSERT INTO {table} (tale_slug, word, occurrences) VALUES {values}"),
        );
        assert_eq!(result.affected_rows, (end - start) as u64);
    }
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\nerror: {err:?}"))
}

fn text_at(result: &RuntimeQueryResult, row: usize, column: &str) -> String {
    match result.result.records[row].get(column) {
        Some(Value::Text(value)) => value.to_string(),
        Some(Value::UnsignedInteger(value)) => value.to_string(),
        Some(Value::Integer(value)) => value.to_string(),
        other => panic!("expected text-like value for {column}, got {other:?}"),
    }
}

fn uint_at(result: &RuntimeQueryResult, row: usize, column: &str) -> u64 {
    match result.result.records[row].get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer for {column}, got {other:?}"),
    }
}
