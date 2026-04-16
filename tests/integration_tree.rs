mod support;

use reddb::application::{ExecuteQueryInput, QueryUseCases};
use reddb::storage::schema::Value;
use reddb::storage::EntityKind;
use reddb::RedDBRuntime;

use support::{checkpoint_and_reopen, PersistentDbPath};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("failed to create in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\nerror: {err:?}"))
}

fn uint(result: &reddb::runtime::RuntimeQueryResult, column: &str) -> u64 {
    match result.result.records[0].get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer for {column}, got {other:?}"),
    }
}

fn bools(result: &reddb::runtime::RuntimeQueryResult, column: &str) -> Vec<bool> {
    result
        .result
        .records
        .iter()
        .map(|record| match record.get(column) {
            Some(Value::Boolean(value)) => *value,
            other => panic!("expected boolean for {column}, got {other:?}"),
        })
        .collect()
}

#[test]
fn test_tree_lifecycle_rebalance_and_delete_subtree() {
    let rt = rt();

    let created = exec(
        &rt,
        "CREATE TREE org IN forest ROOT LABEL company TYPE root PROPERTIES {name: 'Acme'} MAX_CHILDREN 2",
    );
    let root_id = uint(&created, "root_id");

    let a = exec(
        &rt,
        &format!(
            "TREE INSERT INTO forest.org PARENT {root_id} LABEL team TYPE branch PROPERTIES {{name: 'A'}}"
        ),
    );
    let a_id = uint(&a, "node_id");

    let b = exec(
        &rt,
        &format!(
            "TREE INSERT INTO forest.org PARENT {a_id} LABEL squad TYPE branch PROPERTIES {{name: 'B'}}"
        ),
    );
    let b_id = uint(&b, "node_id");

    let _c = exec(
        &rt,
        &format!(
            "TREE INSERT INTO forest.org PARENT {b_id} LABEL leaf TYPE member PROPERTIES {{name: 'C'}}"
        ),
    );

    let valid_before = exec(&rt, "TREE VALIDATE forest.org");
    assert_eq!(bools(&valid_before, "ok"), vec![true]);

    let dry_run = exec(&rt, "TREE REBALANCE forest.org DRY RUN");
    assert!(
        bools(&dry_run, "changed")
            .into_iter()
            .any(|changed| changed),
        "dry-run rebalance should detect at least one structural change"
    );

    let applied = exec(&rt, "TREE REBALANCE forest.org");
    assert!(
        bools(&applied, "changed")
            .into_iter()
            .any(|changed| changed),
        "rebalance should rewrite the skewed tree"
    );

    let valid_after = exec(&rt, "TREE VALIDATE forest.org");
    assert_eq!(bools(&valid_after, "ok"), vec![true]);

    exec(&rt, &format!("TREE DELETE forest.org NODE {a_id}"));

    let valid_after_delete = exec(&rt, "TREE VALIDATE forest.org");
    assert_eq!(bools(&valid_after_delete, "ok"), vec![true]);
}

#[test]
fn test_tree_definition_survives_reopen() {
    let path = PersistentDbPath::new("tree_reopen");
    let rt = path.open_runtime();

    let created = exec(
        &rt,
        "CREATE TREE org IN forest ROOT LABEL company TYPE root PROPERTIES {name: 'Acme'} MAX_CHILDREN 3",
    );
    let root_id = uint(&created, "root_id");
    exec(
        &rt,
        &format!(
            "TREE INSERT INTO forest.org PARENT {root_id} LABEL team TYPE branch PROPERTIES {{name: 'A'}}"
        ),
    );

    let reopened = checkpoint_and_reopen(&path, rt);

    let valid = exec(&reopened, "TREE VALIDATE forest.org");
    assert_eq!(
        bools(&valid, "ok"),
        vec![true],
        "validate after reopen returned: {:?}",
        valid.result.records
    );

    let inserted = exec(
        &reopened,
        &format!(
            "TREE INSERT INTO forest.org PARENT {root_id} LABEL team TYPE branch PROPERTIES {{name: 'B'}}"
        ),
    );
    assert!(uint(&inserted, "node_id") > root_id);
}

#[test]
fn test_generic_edge_insert_rejects_reserved_tree_label() {
    let rt = rt();

    exec(
        &rt,
        "INSERT INTO forest NODE (label, node_type) VALUES ('left', 'Host')",
    );
    exec(
        &rt,
        "INSERT INTO forest NODE (label, node_type) VALUES ('right', 'Host')",
    );

    let ids: Vec<u64> = rt
        .db()
        .store()
        .get_collection("forest")
        .expect("forest collection should exist")
        .query_all(|entity| matches!(entity.kind, EntityKind::GraphNode(_)))
        .into_iter()
        .map(|entity| entity.id.raw())
        .collect();

    assert_eq!(ids.len(), 2, "expected two generic graph nodes");

    let err = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: format!(
                "INSERT INTO forest EDGE (label, from, to, weight) VALUES ('TREE_CHILD', {}, {}, 1.0)",
                ids[0], ids[1]
            ),
        })
        .expect_err("generic edge insert must reject reserved tree label");

    assert!(
        err.to_string().contains("reserved for managed trees"),
        "unexpected error: {err}"
    );
}
