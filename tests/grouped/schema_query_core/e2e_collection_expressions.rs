//! Executable capability matrix: operation × public surface × clean recovery.
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn schema(runtime: &RedDBRuntime) {
    runtime.execute_query("CREATE TABLE orders (id INTEGER PRIMARY KEY, quantity INTEGER NOT NULL CHECK (quantity > 0), price INTEGER DEFAULT=5, final_total INTEGER GENERATED ALWAYS AS (total + 1) STORED, total INTEGER GENERATED ALWAYS AS (price * quantity) STORED CHECK (total < 100))").expect("expression schema");
}

fn value(runtime: &RedDBRuntime, field: &str) -> Value {
    runtime
        .execute_query("SELECT * FROM orders WHERE id = 1")
        .expect("read")
        .result
        .records[0]
        .get(field)
        .expect("field")
        .clone()
}

#[test]
fn expressions_insert_update_index_and_reopen() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("database.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open");
        schema(&runtime);
        runtime
            .execute_query("INSERT INTO orders (id, quantity) VALUES (1, 3)")
            .expect("insert");
        assert_eq!(value(&runtime, "total"), Value::Integer(15));
        assert_eq!(value(&runtime, "final_total"), Value::Integer(16));
        runtime
            .execute_query("CREATE INDEX total_lookup ON orders (total) USING HASH")
            .expect("index");
        runtime
            .execute_query("UPDATE orders SET quantity = 4 WHERE id = 1")
            .expect("update");
        assert_eq!(value(&runtime, "total"), Value::Integer(20));
        assert_eq!(value(&runtime, "final_total"), Value::Integer(21));
        assert_eq!(
            runtime
                .execute_query("SELECT * FROM orders WHERE total = 20")
                .expect("new index key")
                .result
                .records
                .len(),
            1
        );
        assert!(runtime
            .execute_query("SELECT * FROM orders WHERE total = 15")
            .expect("old index key")
            .result
            .records
            .is_empty());
        assert!(runtime
            .execute_query("UPDATE orders SET quantity = 30 WHERE id = 1")
            .is_err());
        assert_eq!(value(&runtime, "quantity"), Value::Integer(4));
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    assert_eq!(value(&runtime, "total"), Value::Integer(20));
    assert!(runtime
        .execute_query("UPDATE orders SET quantity = -1 WHERE id = 1")
        .is_err());
    runtime
        .execute_query("UPDATE orders SET price = 6 WHERE id = 1")
        .expect("recompute after reopen");
    assert_eq!(value(&runtime, "final_total"), Value::Integer(25));
    let contract = runtime
        .db()
        .collection_contract("orders")
        .expect("persisted contract");
    assert_eq!(
        contract.declared_columns[4]
            .generated
            .as_ref()
            .expect("expression")
            .source(),
        "price * quantity"
    );
}

#[test]
fn expressions_reject_invalid_definitions_without_creating_collections() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    for columns in [
        "a INTEGER GENERATED ALWAYS AS (b + 1) STORED, b INTEGER GENERATED ALWAYS AS (a + 1) STORED",
        "a INTEGER GENERATED ALWAYS AS (missing + 1) STORED",
        "a INTEGER CHECK (a + 1)",
        "a INTEGER DEFAULT=1 GENERATED ALWAYS AS (2) STORED",
        "a INTEGER CHECK (random() > 0)",
    ] {
        assert!(runtime.execute_query(&format!("CREATE TABLE invalid ({columns})")).is_err(), "{columns}");
        assert!(runtime.db().collection_contract("invalid").is_none(), "{columns}");
        assert!(runtime.db().store().get_collection("invalid").is_none(), "{columns}");
    }
}

#[test]
fn expressions_check_null_bulk_and_statement_rollback() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    schema(&runtime);
    assert!(runtime
        .execute_query("INSERT INTO orders (id, quantity) VALUES (1, 2), (2, -1)")
        .is_err());
    assert!(runtime
        .execute_query("SELECT * FROM orders")
        .expect("read")
        .result
        .records
        .is_empty());
    runtime
        .execute_query("INSERT INTO orders (id, quantity) VALUES (1, 2), (2, 10)")
        .expect("batch");
    assert!(runtime
        .execute_query("UPDATE orders SET quantity = quantity + 10")
        .is_err());
    assert_eq!(value(&runtime, "quantity"), Value::Integer(2));
    runtime
        .execute_query("CREATE TABLE optional (amount INTEGER CHECK (amount > 0))")
        .expect("nullable check");
    runtime
        .execute_query("INSERT INTO optional (amount) VALUES (NULL)")
        .expect("SQL unknown check passes");
    assert!(runtime
        .execute_query("INSERT INTO optional (amount) VALUES (0)")
        .is_err());
}

#[test]
fn expressions_explicit_transaction_rollback_and_generated_uniqueness() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    schema(&runtime);
    runtime
        .execute_query("INSERT INTO orders (id, quantity) VALUES (1, 2)")
        .expect("insert");
    runtime.execute_query("BEGIN").expect("begin");
    runtime
        .execute_query("UPDATE orders SET quantity = 4 WHERE id = 1")
        .expect("update");
    assert_eq!(value(&runtime, "total"), Value::Integer(20));
    runtime.execute_query("ROLLBACK").expect("rollback");
    assert_eq!(value(&runtime, "total"), Value::Integer(10));
    runtime.execute_query("CREATE TABLE unique_totals (base INTEGER, derived INTEGER GENERATED ALWAYS AS (base * 2) STORED UNIQUE)").expect("unique schema");
    runtime
        .execute_query("INSERT INTO unique_totals (base) VALUES (1), (2)")
        .expect("seed");
    assert!(runtime
        .execute_query("UPDATE unique_totals SET base = 1 WHERE base = 2")
        .is_err());
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM unique_totals WHERE derived = 4")
            .expect("unchanged")
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn expressions_fail_closed_for_alter_without_backfill() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    schema(&runtime);
    for operation in [
        "DROP COLUMN price",
        "RENAME COLUMN price TO cost",
        "ADD COLUMN tax INTEGER",
    ] {
        assert!(runtime
            .execute_query(&format!("ALTER TABLE orders {operation}"))
            .is_err());
    }
}

#[test]
fn expressions_native_insert_patch_and_bulk_share_contract() {
    use reddb::application::{
        CreateRowInput, CreateRowsBatchInput, EntityUseCases, PatchEntityInput,
    };
    use reddb::serde_json::json;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    schema(&runtime);
    let input = |id, quantity| CreateRowInput {
        collection: "orders".into(),
        fields: vec![
            ("id".into(), Value::Integer(id)),
            ("quantity".into(), Value::Integer(quantity)),
        ],
        metadata: Vec::new(),
        node_links: Vec::new(),
        vector_links: Vec::new(),
    };
    let api = EntityUseCases::new(&runtime);
    let created = api.create_row(input(1, 2)).expect("native insert");
    assert_eq!(value(&runtime, "total"), Value::Integer(10));
    api.patch(PatchEntityInput {
        collection: "orders".into(),
        id: created.id,
        payload: json!({"fields": json!({"quantity": 3})}),
        operations: Vec::new(),
    })
    .expect("native patch");
    assert_eq!(value(&runtime, "total"), Value::Integer(15));
    assert!(api
        .create_rows_batch(CreateRowsBatchInput {
            collection: "orders".into(),
            rows: vec![input(2, 4), input(3, -1)],
            suppress_events: false
        })
        .is_err());
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM orders")
            .expect("no partial batch")
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn expressions_upsert_recomputes_final_record() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    schema(&runtime);
    runtime
        .execute_query("INSERT INTO orders (id, quantity) VALUES (1, 2)")
        .expect("seed");
    runtime.execute_query("INSERT INTO orders (id, quantity) VALUES (1, 3) ON CONFLICT (id) DO UPDATE SET quantity = 4").expect("upsert");
    assert_eq!(value(&runtime, "total"), Value::Integer(20));
    assert!(runtime.execute_query("INSERT INTO orders (id, quantity) VALUES (1, 3) ON CONFLICT (id) DO UPDATE SET quantity = 30").is_err());
    assert_eq!(value(&runtime, "total"), Value::Integer(20));
}

#[test]
fn expressions_public_example_schemas_execute() {
    let examples = [
        include_str!("../../../examples/collection-expressions/commerce.rql"),
        include_str!("../../../examples/collection-expressions/fraud-events.rql"),
        include_str!("../../../examples/collection-expressions/agent-knowledge.rql"),
    ];
    for source in examples {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        for statement in source
            .split(';')
            .map(str::trim)
            .filter(|statement| !statement.is_empty())
        {
            runtime.execute_query(statement).expect(statement);
        }
    }
}

#[test]
fn expressions_generated_outputs_enforce_types_nullability_and_overflow() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    schema(&runtime);
    runtime
        .execute_query("INSERT INTO orders (id, quantity, total) VALUES (1, 2, 999)")
        .expect("derived input is recomputed");
    assert_eq!(value(&runtime, "total"), Value::Integer(10));
    assert!(runtime
        .execute_query("UPDATE orders SET price = 9223372036854775807 WHERE id = 1")
        .is_err());
    assert_eq!(value(&runtime, "price"), Value::Integer(5));
    runtime.execute_query("CREATE TABLE required_output (base INTEGER, derived INTEGER NOT NULL GENERATED ALWAYS AS (base + 1) STORED)").expect("schema");
    assert!(runtime
        .execute_query("INSERT INTO required_output (base) VALUES (NULL)")
        .is_err());
    runtime.execute_query("CREATE TABLE conditionals (base INTEGER, derived INTEGER GENERATED ALWAYS AS (CASE WHEN base BETWEEN 1 AND 3 THEN base + 1 ELSE 0 END) STORED CHECK (derived IN (0, 2, 3, 4)))").expect("conditional schema");
    runtime
        .execute_query("INSERT INTO conditionals (base) VALUES (2)")
        .expect("conditional expression");
    let rows = runtime
        .execute_query("SELECT derived FROM conditionals")
        .expect("read");
    assert_eq!(
        rows.result.records[0].get("derived"),
        Some(&Value::Integer(3))
    );
}
