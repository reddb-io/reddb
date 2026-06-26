#[path = "../../support/mod.rs"]
mod support;

use support::{checkpoint_and_reopen, PersistentDbPath};

use reddb::application::{ExecuteQueryInput, QueryUseCases};
use reddb::runtime::RuntimeQueryResult;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

const NODE_COUNT: usize = 14_256;
const EDGE_COUNT: usize = 46_202;
const TABLE_ROWS: usize = 53_946;
const NODE_BATCH: usize = 512;
const EDGE_BATCH: usize = 512;
const TABLE_BATCH: usize = 1_024;

#[test]
#[ignore = "persistent grimms-scale fixture"]
fn persistent_grimms_scale_graph_then_table_reopens_without_checksum_or_schema_loss() {
    let path = PersistentDbPath::new("grimms_scale_graph_then_table");
    let rt = path.open_runtime();

    let node_ids = insert_graph_nodes(&rt, "tales_grimms_scale", NODE_COUNT, NODE_BATCH);
    insert_graph_edges(&rt, "tales_grimms_scale", &node_ids, EDGE_COUNT, EDGE_BATCH);

    exec(
        &rt,
        "CREATE TABLE tale_words_grimms_scale (tale_slug TEXT, word TEXT, occurrences INTEGER)",
    );
    insert_word_rows(&rt, "tale_words_grimms_scale", TABLE_ROWS, TABLE_BATCH);

    let rt = checkpoint_and_reopen(&path, rt);

    let total = exec(&rt, "SELECT count(*) AS total FROM tale_words_grimms_scale");
    assert_eq!(uint_at(&total, 0, "total"), TABLE_ROWS as u64);

    let first = exec(
        &rt,
        "SELECT word, occurrences FROM tale_words_grimms_scale WHERE word = 'word_000000'",
    );
    assert_eq!(first.result.records.len(), 1);
    assert_eq!(text_at(&first, 0, "word"), "word_000000");
    assert_eq!(uint_at(&first, 0, "occurrences"), 1);

    let last = exec(
        &rt,
        "SELECT word, occurrences FROM tale_words_grimms_scale WHERE word = 'word_053945'",
    );
    assert_eq!(last.result.records.len(), 1);
    assert_eq!(text_at(&last, 0, "word"), "word_053945");
    assert_eq!(uint_at(&last, 0, "occurrences"), 6);

    let path = exec(
        &rt,
        &format!(
            "GRAPH SHORTEST_PATH '{}' TO '{}' ALGORITHM dijkstra",
            node_ids[0], node_ids[10]
        ),
    );
    assert_eq!(uint_at(&path, 0, "hop_count"), 10);
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
            .map(|i| format!("('node_{i:05}', 'character', 'Node {i}')"))
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
            ids.push(uint_at(&result, row, "rid"));
        }
    }
    ids
}

fn insert_graph_edges(
    rt: &RedDBRuntime,
    collection: &str,
    node_ids: &[u64],
    count: usize,
    batch_size: usize,
) {
    for start in (0..count).step_by(batch_size) {
        let end = (start + batch_size).min(count);
        let values = (start..end)
            .map(|i| {
                let from_idx = i % node_ids.len();
                let to_idx = if i + 1 < node_ids.len() {
                    i + 1
                } else {
                    (i * 37 + 97) % node_ids.len()
                };
                let label = if i % 3 == 0 { "HAS_TRAIT" } else { "NEXT" };
                format!(
                    "('{label}', {}, {}, 1.0, 'evidence {i}')",
                    node_ids[from_idx], node_ids[to_idx]
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let result = exec(
            rt,
            &format!(
                "INSERT INTO {collection} EDGE (label, from, to, weight, evidence) VALUES {values}"
            ),
        );
        assert_eq!(result.affected_rows, (end - start) as u64);
    }
}

fn insert_word_rows(rt: &RedDBRuntime, table: &str, count: usize, batch_size: usize) {
    for start in (0..count).step_by(batch_size) {
        let end = (start + batch_size).min(count);
        let values = (start..end)
            .map(|i| format!("('tale_{:03}', 'word_{:06}', {})", i % 206, i, (i % 10) + 1))
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
