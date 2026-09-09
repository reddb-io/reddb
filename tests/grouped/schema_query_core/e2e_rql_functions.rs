//! Stored functions are invoker-scoped database primitives, with atomic data bodies.
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn execute(runtime: &RedDBRuntime, source: &str) -> reddb::RuntimeQueryResult {
    runtime
        .execute_query(source)
        .unwrap_or_else(|cause| panic!("{source}: {cause}"))
}

fn scalar(runtime: &RedDBRuntime, source: &str) -> Value {
    let result = execute(runtime, source);
    assert_eq!(result.result.records.len(), 1, "{source}");
    result.result.records[0]
        .get(&result.result.columns[0])
        .expect("scalar field")
        .clone()
}

fn accounts(runtime: &RedDBRuntime) {
    execute(runtime, "CREATE TABLE accounts (id INTEGER PRIMARY KEY, amount INTEGER NOT NULL CHECK (amount >= 0))");
    execute(runtime, "INSERT INTO accounts (id, amount) VALUES (1, 10)");
}

fn amount(runtime: &RedDBRuntime) -> Value {
    scalar(runtime, "SELECT amount FROM accounts WHERE id = 1")
}

fn define(runtime: &RedDBRuntime, signature: &str, body: &str) {
    execute(
        runtime,
        &format!(
            "CREATE FUNCTION {signature} AS '{}'",
            body.replace('\'', "''")
        ),
    );
}

#[test]
fn pure_typed_parameters_expressions_and_injection_as_data() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    define(
        &runtime,
        "total(price INTEGER, quantity INTEGER) RETURNS INTEGER EFFECT PURE",
        "SELECT $1 * $2",
    );
    assert_eq!(scalar(&runtime, "CALL total(2 + 1, 4)"), Value::Integer(12));
    let result = runtime
        .execute_query_with_params(
            "CALL total($1, $2)",
            &[Value::Integer(7), Value::Integer(6)],
        )
        .expect("bound CALL");
    assert_eq!(
        result.result.records[0].get("value"),
        Some(&Value::Integer(42))
    );
    assert!(runtime.execute_query("CALL total(1)").is_err());
    assert!(runtime.execute_query("CALL total('bad', 2)").is_err());
    define(
        &runtime,
        "echo(input TEXT) RETURNS TEXT EFFECT PURE",
        "SELECT $1",
    );
    let payload = "'; DROP FUNCTION total; SELECT '";
    let result = runtime
        .execute_query_with_params("CALL echo($1)", &[Value::text(payload)])
        .expect("data");
    assert_eq!(
        result.result.records[0].get("value"),
        Some(&Value::text(payload))
    );
    assert_eq!(scalar(&runtime, "CALL total(2, 3)"), Value::Integer(6));
}

#[test]
fn read_body_accepts_sparse_arguments_per_statement() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    accounts(&runtime);
    define(&runtime, "lookup(unused INTEGER, item INTEGER) RETURNS TABLE (id INTEGER, amount INTEGER) EFFECT READ", "SELECT $1; SELECT id, amount FROM accounts WHERE id = $2");
    let result = execute(&runtime, "CALL lookup(999, 1)");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("amount"),
        Some(&Value::Integer(10))
    );
    assert!(execute(&runtime, "CALL lookup(999, 2)")
        .result
        .records
        .is_empty());
}

#[test]
fn write_body_commits_and_rolls_back_statement_and_return_failures() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    accounts(&runtime);
    define(&runtime, "adjust(delta INTEGER, item INTEGER) RETURNS TABLE (id INTEGER, amount INTEGER) EFFECT WRITE", "UPDATE accounts SET amount = amount + $1 WHERE id = $2; SELECT id, amount FROM accounts WHERE id = $2");
    execute(&runtime, "CALL adjust(5, 1)");
    assert_eq!(amount(&runtime), Value::Integer(15));
    define(&runtime, "fail_statement() RETURNS INTEGER EFFECT WRITE", "UPDATE accounts SET amount = 20 WHERE id = 1; UPDATE accounts SET amount = -1 WHERE id = 1; SELECT 1");
    assert!(runtime.execute_query("CALL fail_statement()").is_err());
    assert_eq!(amount(&runtime), Value::Integer(15));
    define(
        &runtime,
        "fail_return() RETURNS INTEGER EFFECT WRITE",
        "UPDATE accounts SET amount = 30 WHERE id = 1; SELECT 'invalid'",
    );
    assert!(runtime.execute_query("CALL fail_return()").is_err());
    assert_eq!(amount(&runtime), Value::Integer(15));
    execute(&runtime, "CALL adjust(1, 1)");
    assert_eq!(amount(&runtime), Value::Integer(16));
}

#[test]
fn ambient_transaction_savepoint_preserves_caller_and_outer_rollback() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    accounts(&runtime);
    define(
        &runtime,
        "bad() RETURNS INTEGER EFFECT WRITE",
        "UPDATE accounts SET amount = 30 WHERE id = 1; SELECT 'invalid'",
    );
    define(
        &runtime,
        "good() RETURNS INTEGER EFFECT WRITE",
        "UPDATE accounts SET amount = 40 WHERE id = 1; SELECT 1",
    );
    execute(&runtime, "BEGIN");
    execute(&runtime, "UPDATE accounts SET amount = 20 WHERE id = 1");
    assert!(runtime.execute_query("CALL bad()").is_err());
    assert_eq!(amount(&runtime), Value::Integer(20));
    execute(&runtime, "CALL good()");
    assert_eq!(amount(&runtime), Value::Integer(40));
    execute(&runtime, "ROLLBACK");
    assert_eq!(amount(&runtime), Value::Integer(10));
}

#[test]
fn definition_lifecycle_export_and_clean_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("functions.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open");
        define(&runtime, "answer() RETURNS INTEGER EFFECT PURE", "SELECT 1");
        assert!(runtime
            .execute_query("CREATE FUNCTION answer() RETURNS INTEGER EFFECT PURE AS 'SELECT 2'")
            .is_err());
        assert_eq!(scalar(&runtime, "CALL answer()"), Value::Integer(1));
        execute(
            &runtime,
            "ALTER FUNCTION answer() RETURNS INTEGER EFFECT PURE AS 'SELECT 2'",
        );
        assert_eq!(scalar(&runtime, "CALL answer()"), Value::Integer(2));
        let exported = execute(&runtime, "SHOW FUNCTION answer");
        let Value::Text(ddl) = exported.result.records[0].get("ddl").expect("DDL") else {
            panic!("DDL text");
        };
        let imported = RedDBRuntime::in_memory().expect("import target");
        execute(&imported, ddl);
        assert_eq!(scalar(&imported, "CALL answer()"), Value::Integer(2));
        assert!(
            !format!("{:?}", execute(&runtime, "SHOW CONFIG").result).contains("function_catalog")
        );
    }
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
        assert_eq!(scalar(&runtime, "CALL answer()"), Value::Integer(2));
        execute(&runtime, "DROP FUNCTION answer");
        execute(&runtime, "DROP FUNCTION IF EXISTS answer");
        assert!(runtime.execute_query("CALL answer()").is_err());
    }
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen dropped");
    assert!(execute(&runtime, "SHOW FUNCTIONS")
        .result
        .records
        .is_empty());
}

#[test]
fn invalid_effects_and_control_flow_fail_before_catalog_mutation() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    for (effect, body) in [
        ("PURE", "SELECT id FROM accounts"),
        ("READ", "UPDATE accounts SET amount = 1"),
        ("WRITE", "BEGIN"),
        ("WRITE", "CALL recursive()"),
        ("WRITE", "CREATE TABLE nested (id INTEGER)"),
        ("PURE", "SELECT random()"),
        ("PURE", "SELECT count(1)"),
        ("PURE", "SELECT now()"),
        ("PURE", "SELECT unknown_external()"),
        ("PURE", "SELECT $2"),
        ("PURE", "SELECT (SELECT id FROM accounts)"),
    ] {
        let ddl = format!(
            "CREATE FUNCTION invalid(arg INTEGER) RETURNS INTEGER EFFECT {effect} AS '{body}'"
        );
        assert!(runtime.execute_query(&ddl).is_err(), "{ddl}");
    }
    assert!(execute(&runtime, "SHOW FUNCTIONS")
        .result
        .records
        .is_empty());
    execute(&runtime, "BEGIN");
    assert!(runtime
        .execute_query("CREATE FUNCTION invalid() RETURNS INTEGER EFFECT PURE AS 'SELECT 1'")
        .is_err());
    execute(&runtime, "ROLLBACK");
}

#[test]
fn tenant_catalog_definitions_are_separate() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    execute(&runtime, "SET TENANT 'alpha'");
    define(&runtime, "answer() RETURNS INTEGER EFFECT PURE", "SELECT 1");
    execute(&runtime, "SET TENANT 'beta'");
    assert!(runtime.execute_query("CALL answer()").is_err());
    assert!(execute(&runtime, "SHOW FUNCTIONS")
        .result
        .records
        .is_empty());
    define(&runtime, "answer() RETURNS INTEGER EFFECT PURE", "SELECT 2");
    assert_eq!(scalar(&runtime, "CALL answer()"), Value::Integer(2));
    execute(&runtime, "SET TENANT 'alpha'");
    assert_eq!(scalar(&runtime, "CALL answer()"), Value::Integer(1));
    execute(&runtime, "SET TENANT NULL");
}

#[test]
fn invoker_execute_does_not_elevate_body_permissions() {
    use reddb::auth::{AuthConfig, AuthStore, Role};
    use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
    use std::sync::Arc;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    accounts(&runtime);
    define(
        &runtime,
        "lookup() RETURNS TABLE (id INTEGER) EFFECT READ",
        "SELECT id FROM accounts",
    );
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    store
        .create_user("alice", "test-password", Role::Read)
        .expect("user");
    store.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);
    let policy = reddb::auth::policies::Policy::from_json_str(r#"{"id":"invoke","version":1,"statements":[{"effect":"allow","actions":["execute"],"resources":["function:lookup"]}]}"#).expect("policy");
    store.put_policy(policy).expect("save policy");
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("alice")),
            "invoke",
        )
        .expect("attach");
    runtime.set_auth_store(Arc::clone(&store));
    set_current_auth_identity("alice".to_string(), Role::Read);
    let denied = runtime
        .execute_query("CALL lookup()")
        .expect_err("EXECUTE cannot grant SELECT")
        .to_string();
    clear_current_auth_identity();
    assert!(denied.contains("accounts"), "{denied}");
    let policy = reddb::auth::policies::Policy::from_json_str(r#"{"id":"invoke","version":1,"statements":[{"effect":"allow","actions":["execute"],"resources":["function:lookup"]},{"effect":"allow","actions":["select"],"resources":["table:accounts","table:accounts:*"]}]}"#).expect("policy");
    store.put_policy(policy).expect("replace policy");
    set_current_auth_identity("alice".to_string(), Role::Read);
    let result = runtime.execute_query("CALL lookup()");
    clear_current_auth_identity();
    assert_eq!(result.expect("invoker can read").result.records.len(), 1);
    let policy = reddb::auth::policies::Policy::from_json_str(r#"{"id":"invoke","version":1,"statements":[{"effect":"allow","actions":["*"],"resources":["*"]},{"effect":"deny","actions":["execute"],"resources":["function:lookup"]}]}"#).expect("policy");
    store.put_policy(policy).expect("replace policy");
    set_current_auth_identity("alice".to_string(), Role::Read);
    let denied = runtime.execute_query("CALL lookup()");
    let listed = runtime.execute_query("SHOW FUNCTIONS");
    clear_current_auth_identity();
    assert!(denied.is_err());
    assert!(listed
        .expect("list filters denied functions")
        .result
        .records
        .is_empty());
}

#[test]
fn invalid_persisted_catalog_refuses_runtime_open() {
    use reddb::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
    use std::sync::Arc;
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("invalid.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open");
        define(&runtime, "answer() RETURNS INTEGER EFFECT PURE", "SELECT 1");
        // Low-level embedded store access is trusted; inject malformed disk metadata.
        runtime
            .db()
            .store()
            .insert_auto(
                "red_config",
                UnifiedEntity::new(
                    EntityId::new(0),
                    EntityKind::TableRow {
                        table: Arc::from("red_config"),
                        row_id: 0,
                    },
                    EntityData::Row(RowData {
                        columns: Vec::new(),
                        schema: None,
                        named: Some(
                            [
                                ("key".to_string(), Value::text("red.rql.function_catalog")),
                                ("value".to_string(), Value::text("not-json")),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                    }),
                ),
            )
            .expect("inject malformed catalog");
        runtime.db().flush().expect("flush");
    }
    let cause = match RedDBRuntime::with_options(RedDBOptions::persistent(&path)) {
        Ok(_) => panic!("invalid catalog must prevent opening"),
        Err(cause) => cause.to_string(),
    };
    assert!(cause.contains("catalog"), "{cause}");
}

#[test]
fn graph_read_binds_predicate_instead_of_reparsing_source() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    execute(
        &runtime,
        "INSERT INTO social NODE (label, name) VALUES ('Person', 'alice')",
    );
    execute(
        &runtime,
        "INSERT INTO social NODE (label, name) VALUES ('Person', 'bob')",
    );
    define(
        &runtime,
        "people(wanted TEXT) RETURNS TABLE (name TEXT) EFFECT READ",
        "MATCH (n:Person) WHERE n.name = $1 RETURN n.name AS name",
    );
    let result = execute(&runtime, "CALL people('alice')");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("name"),
        Some(&Value::text("alice"))
    );
    assert!(execute(&runtime, "CALL people('absent')")
        .result
        .records
        .is_empty());
}

#[test]
fn table_and_queue_writes_rollback_together() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    accounts(&runtime);
    execute(&runtime, "CREATE QUEUE jobs");
    define(&runtime, "enqueue() RETURNS INTEGER EFFECT WRITE", "UPDATE accounts SET amount = 25 WHERE id = 1; QUEUE PUSH jobs {payload: 'notice'}; SELECT 1");
    define(&runtime, "bad_enqueue() RETURNS INTEGER EFFECT WRITE", "UPDATE accounts SET amount = 35 WHERE id = 1; QUEUE PUSH jobs {payload: 'discard'}; SELECT 'invalid'");
    assert!(runtime.execute_query("CALL bad_enqueue()").is_err());
    assert_eq!(amount(&runtime), Value::Integer(10));
    let empty = execute(&runtime, "QUEUE PEEK jobs");
    assert!(empty.result.records.is_empty());
    execute(&runtime, "CALL enqueue()");
    assert_eq!(amount(&runtime), Value::Integer(25));
    let result = execute(&runtime, "QUEUE PEEK jobs");
    assert_eq!(result.result.records.len(), 1);
}

#[test]
fn pure_function_needs_execute_without_a_fictitious_any_table_grant() {
    use reddb::auth::{AuthConfig, AuthStore, Role};
    use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
    use std::sync::Arc;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    define(
        &runtime,
        "answer() RETURNS INTEGER EFFECT PURE",
        "SELECT 42",
    );
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    store
        .create_user("alice", "test-password", Role::Read)
        .expect("user");
    store.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);
    let policy = reddb::auth::policies::Policy::from_json_str(r#"{"id":"invoke","version":1,"statements":[{"effect":"allow","actions":["execute"],"resources":["function:answer"]}]}"#).expect("policy");
    store.put_policy(policy).expect("policy");
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("alice")),
            "invoke",
        )
        .expect("attach");
    runtime.set_auth_store(store);
    set_current_auth_identity("alice".to_string(), Role::Read);
    let result = runtime.execute_query("CALL answer()");
    let aggregate = runtime.execute_query("SELECT count(1)");
    clear_current_auth_identity();
    assert_eq!(
        result
            .expect("execute alone admits pure function")
            .result
            .records[0]
            .get("value"),
        Some(&Value::Integer(42))
    );
    assert!(
        aggregate.is_err(),
        "aggregate still needs underlying read permission"
    );
}

#[test]
fn configuration_surface_hides_and_protects_function_catalog() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    define(
        &runtime,
        "answer() RETURNS INTEGER EFFECT PURE",
        "SELECT 42",
    );
    assert!(runtime
        .execute_query("SET CONFIG red.rql.function_catalog = '[]'")
        .is_err());
    assert_eq!(
        scalar(&runtime, "SELECT CONFIG('red.rql.function_catalog')"),
        Value::Null
    );
    assert!(execute(&runtime, "SHOW CONFIG red.rql.function_catalog")
        .result
        .records
        .is_empty());
    assert_eq!(scalar(&runtime, "CALL answer()"), Value::Integer(42));
}

#[test]
fn function_reads_follow_row_policies_and_schema_qualified_grants() {
    use reddb::auth::{AuthConfig, AuthStore, Role};
    use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
    use std::sync::Arc;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    accounts(&runtime);
    execute(&runtime, "INSERT INTO accounts (id, amount) VALUES (2, 20)");
    define(
        &runtime,
        "public.visible_accounts() RETURNS TABLE (id INTEGER) EFFECT READ",
        "SELECT id FROM accounts",
    );
    execute(
        &runtime,
        "CREATE POLICY first_only ON accounts USING (id = 1)",
    );
    execute(&runtime, "ALTER TABLE accounts ENABLE ROW LEVEL SECURITY");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    store
        .create_user("alice", "test-password", Role::Read)
        .expect("user");
    store
        .create_user("admin", "test-password", Role::Admin)
        .expect("admin");
    runtime.set_auth_store(store);
    set_current_auth_identity("admin".to_string(), Role::Admin);
    execute(
        &runtime,
        "GRANT EXECUTE ON FUNCTION public.visible_accounts TO alice",
    );
    execute(&runtime, "GRANT SELECT ON TABLE accounts TO alice");
    set_current_auth_identity("alice".to_string(), Role::Read);
    let result = runtime.execute_query("CALL public.visible_accounts()");
    clear_current_auth_identity();
    let result = result.expect("invoker follows row policy");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(result.result.records[0].get("id"), Some(&Value::Integer(1)));
}

#[test]
fn failed_catalog_flush_blocks_function_operations_until_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("functions.rdb");
    let retained = directory.path().join("retained.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open");
        define(&runtime, "answer() RETURNS INTEGER EFFECT PURE", "SELECT 1");
        // Force a real publication/rename error on this test's own artifact.
        std::fs::rename(&path, &retained).expect("retain test artifact");
        std::fs::create_dir(&path).expect("block artifact publication with directory");
        let failed = runtime
            .execute_query("ALTER FUNCTION answer() RETURNS INTEGER EFFECT PURE AS 'SELECT 2'");
        let blocked = runtime.execute_query("CALL answer()");
        std::fs::remove_dir(&path).expect("remove empty test blocker");
        std::fs::rename(&retained, &path).expect("restore retained artifact");
        assert!(failed
            .expect_err("publication must fail")
            .to_string()
            .contains("indeterminate"));
        assert!(blocked
            .expect_err("catalog may not serve stale definition")
            .to_string()
            .contains("indeterminate"));
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .expect("reopen reconciles catalog");
    assert!(
        matches!(scalar(&runtime, "CALL answer()"), Value::Integer(1 | 2)),
        "failed DDL may recover either complete catalog"
    );
}

fn work_budget(runtime: &RedDBRuntime, work_max: u64) {
    execute(
        runtime,
        &format!("SET CONFIG functions.execution.work_max = {work_max}"),
    );
}

fn budget_rows(runtime: &RedDBRuntime) {
    execute(
        runtime,
        "CREATE TABLE budget_rows (id INTEGER, bucket INTEGER)",
    );
    let values = (0..64)
        .map(|id| format!("({id}, 1)"))
        .collect::<Vec<_>>()
        .join(",");
    execute(
        runtime,
        &format!("INSERT INTO budget_rows (id, bucket) VALUES {values}"),
    );
}

fn exhausted(runtime: &RedDBRuntime, source: &str) {
    let error = runtime
        .execute_query(source)
        .expect_err("CALL must fail instead of returning partial data");
    assert!(
        error.to_string().contains("execution work_max exceeded"),
        "{source}: {error}"
    );
}

#[test]
fn function_budget_accumulates_across_statements_and_resets_between_calls() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    define(&runtime, "one() RETURNS INTEGER EFFECT PURE", "SELECT 1");
    define(
        &runtime,
        "three() RETURNS INTEGER EFFECT PURE",
        "SELECT 1; SELECT 2; SELECT 3",
    );
    work_budget(&runtime, 2);
    exhausted(&runtime, "CALL three()");
    assert_eq!(scalar(&runtime, "CALL one()"), Value::Integer(1));
    assert_eq!(scalar(&runtime, "CALL one()"), Value::Integer(1));
    work_budget(&runtime, 3);
    assert_eq!(scalar(&runtime, "CALL three()"), Value::Integer(3));
}

#[test]
fn function_budget_counts_rejected_scan_rows_and_aggregate_inputs() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    budget_rows(&runtime);
    define(
        &runtime,
        "filtered() RETURNS TABLE (id INTEGER) EFFECT READ",
        "SELECT id FROM budget_rows WHERE id + 1 = 9999",
    );
    define(
        &runtime,
        "counted() RETURNS INTEGER EFFECT READ",
        "SELECT COUNT(*) FROM budget_rows",
    );
    define(
        &runtime,
        "grouped() RETURNS TABLE (bucket INTEGER) EFFECT READ",
        "SELECT bucket, COUNT(*) AS total FROM budget_rows GROUP BY bucket",
    );
    work_budget(&runtime, 20);
    for name in ["filtered", "counted", "grouped"] {
        exhausted(&runtime, &format!("CALL {name}()"));
    }
    // Ordinary SQL has no CALL budget, including after a failed scan.
    assert_eq!(
        scalar(&runtime, "SELECT COUNT(*) FROM budget_rows"),
        Value::Integer(64)
    );
    work_budget(&runtime, 200);
    assert_eq!(scalar(&runtime, "CALL counted()"), Value::Integer(64));
    assert!(execute(&runtime, "CALL filtered()")
        .result
        .records
        .is_empty());
    assert_eq!(
        execute(&runtime, "CALL grouped()").result.records[0].get("bucket"),
        Some(&Value::Integer(1))
    );
}

#[test]
fn function_budget_interrupts_join_expansion_before_return_row_limit() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    budget_rows(&runtime);
    define(
        &runtime,
        "product() RETURNS TABLE (id INTEGER) EFFECT READ",
        "SELECT a.id AS id FROM budget_rows a CROSS JOIN budget_rows b",
    );
    define(
        &runtime,
        "matched() RETURNS TABLE (id INTEGER) EFFECT READ",
        "SELECT a.id AS id FROM budget_rows a JOIN budget_rows b ON a.bucket = b.bucket",
    );
    // Inputs fit; the 4,096-row expansion does not.
    work_budget(&runtime, 300);
    exhausted(&runtime, "CALL product()");
    exhausted(&runtime, "CALL matched()");
    work_budget(&runtime, 20_000);
    assert_eq!(
        execute(&runtime, "CALL product()").result.records.len(),
        4096
    );
    assert_eq!(
        execute(&runtime, "CALL matched()").result.records.len(),
        4096
    );
}

#[test]
fn function_budget_rolls_back_writes_in_owned_and_caller_transactions() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    accounts(&runtime);
    budget_rows(&runtime);
    define(
        &runtime,
        "expensive_write() RETURNS INTEGER EFFECT WRITE",
        "UPDATE accounts SET amount = amount + 1 WHERE id = 1; SELECT COUNT(*) FROM budget_rows",
    );
    work_budget(&runtime, 20);
    exhausted(&runtime, "CALL expensive_write()");
    assert_eq!(amount(&runtime), Value::Integer(10));

    execute(&runtime, "BEGIN");
    execute(&runtime, "UPDATE accounts SET amount = 20 WHERE id = 1");
    execute(&runtime, "SAVEPOINT user_savepoint");
    exhausted(&runtime, "CALL expensive_write()");
    assert_eq!(amount(&runtime), Value::Integer(20));
    execute(&runtime, "ROLLBACK TO SAVEPOINT user_savepoint");
    execute(&runtime, "COMMIT");
    assert_eq!(amount(&runtime), Value::Integer(20));
}

#[test]
fn function_budget_charges_mutations_and_table_return_rows() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    accounts(&runtime);
    define(
        &runtime,
        "write_only() RETURNS TABLE (id INTEGER) EFFECT WRITE",
        "UPDATE accounts SET amount = amount + 1 WHERE id = 1; SELECT 1 AS id",
    );
    // Two statements + one affected row fit, return normalization does not.
    work_budget(&runtime, 3);
    exhausted(&runtime, "CALL write_only()");
    assert_eq!(amount(&runtime), Value::Integer(10));
    work_budget(&runtime, 4);
    assert_eq!(
        execute(&runtime, "CALL write_only()").result.records.len(),
        1
    );
    assert_eq!(amount(&runtime), Value::Integer(11));
}

#[test]
fn zero_function_budget_is_rejected_without_opening_a_transaction() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    accounts(&runtime);
    define(
        &runtime,
        "read_amount() RETURNS INTEGER EFFECT READ",
        "SELECT amount FROM accounts WHERE id = 1",
    );
    work_budget(&runtime, 0);
    let error = runtime
        .execute_query("CALL read_amount()")
        .expect_err("zero is not unlimited");
    assert!(error.to_string().contains("must be positive"));
    execute(&runtime, "BEGIN");
    execute(&runtime, "ROLLBACK");
    work_budget(&runtime, 100);
    assert_eq!(scalar(&runtime, "CALL read_amount()"), Value::Integer(10));
}
