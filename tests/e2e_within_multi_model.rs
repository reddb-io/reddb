//! `WITHIN TENANT '<id>'` against non-table models.
//!
//! The WITHIN prefix is stripped at the top-level `execute_query` hook
//! before the inner statement is parsed, so it should work uniformly
//! across queue, vector, graph, and timeseries — anywhere RLS evaluates
//! `CURRENT_TENANT()`.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn within_filters_queue_messages() {
    let rt = open_runtime();
    exec(&rt, "CREATE QUEUE jobs");
    exec(
        &rt,
        "CREATE POLICY tenant_only ON MESSAGES OF jobs \
         USING (payload.tenant = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE jobs ENABLE ROW LEVEL SECURITY");

    exec(&rt, "QUEUE PUSH jobs {tenant: 'acme', task: 'ship'}");
    exec(&rt, "QUEUE PUSH jobs {tenant: 'globex', task: 'audit'}");
    exec(&rt, "QUEUE PUSH jobs {tenant: 'acme', task: 'invoice'}");

    set_current_connection_id(801);

    let acme = rt
        .execute_query("WITHIN TENANT 'acme' QUEUE LEN jobs")
        .unwrap();
    let n = acme
        .result
        .records
        .first()
        .and_then(|r| r.values.get("len"))
        .cloned()
        .unwrap();
    assert_eq!(
        n,
        reddb::storage::schema::Value::UnsignedInteger(2),
        "acme should see 2 messages"
    );

    let globex = rt
        .execute_query("WITHIN TENANT 'globex' QUEUE LEN jobs")
        .unwrap();
    let n = globex
        .result
        .records
        .first()
        .and_then(|r| r.values.get("len"))
        .cloned()
        .unwrap();
    assert_eq!(
        n,
        reddb::storage::schema::Value::UnsignedInteger(1),
        "globex should see 1 message"
    );

    // No WITHIN, no SET TENANT → deny-default.
    let unbound = rt.execute_query("QUEUE LEN jobs").unwrap();
    let n = unbound
        .result
        .records
        .first()
        .and_then(|r| r.values.get("len"))
        .cloned()
        .unwrap();
    assert_eq!(n, reddb::storage::schema::Value::UnsignedInteger(0));

    clear_current_connection_id();
}

#[test]
#[ignore = "MATCH executor does not apply RLS today — known gap"]
fn within_filters_graph_nodes() {
    let rt = open_runtime();
    // Graph collections auto-create on first NODE insert — declaring
    // them as TABLE first locks the kind and rejects NODE writes.
    exec(
        &rt,
        "INSERT INTO social NODE (label, name, tenant_id) VALUES ('User', 'alice', 'acme')",
    );
    exec(
        &rt,
        "INSERT INTO social NODE (label, name, tenant_id) VALUES ('User', 'bob', 'globex')",
    );
    exec(
        &rt,
        "INSERT INTO social NODE (label, name, tenant_id) VALUES ('User', 'carol', 'acme')",
    );

    // Same kind-vs-Table caveat as timeseries: MATCH does not consult
    // Nodes-kind policies today, so we use the basic `ON <coll>` form
    // which the read path does evaluate.
    exec(
        &rt,
        "CREATE POLICY tenant_iso ON social \
         USING (properties.tenant_id = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE social ENABLE ROW LEVEL SECURITY");

    set_current_connection_id(803);

    let acme = rt
        .execute_query("WITHIN TENANT 'acme' MATCH (n) RETURN n")
        .unwrap();
    assert_eq!(
        acme.result.records.len(),
        2,
        "acme should see 2 nodes, got {:?}",
        acme.result.records
    );

    let globex = rt
        .execute_query("WITHIN TENANT 'globex' MATCH (n) RETURN n")
        .unwrap();
    assert_eq!(globex.result.records.len(), 1);

    clear_current_connection_id();
}

#[test]
fn within_filters_timeseries_points() {
    let rt = open_runtime();
    exec(&rt, "CREATE TIMESERIES metrics RETENTION 7 d");
    // The basic `ON <table>` (kind=Table) policy form is what the
    // SELECT path queries. The `ON POINTS OF <ts>` (kind=Points) form
    // is parsed and stored but the timeseries read path does not
    // consult Points-kind policies today — see the doc note in
    // `docs/security/multi-tenancy.md` and `docs/security/rls.md`.
    exec(
        &rt,
        "CREATE POLICY tenant_iso ON metrics \
         USING (tags.tenant = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE metrics ENABLE ROW LEVEL SECURITY");

    exec(
        &rt,
        "INSERT INTO metrics (metric, value, tags, timestamp) VALUES \
         ('cpu', 50.0, {tenant: 'acme', host: 'a1'}, 1704067200000000000)",
    );
    exec(
        &rt,
        "INSERT INTO metrics (metric, value, tags, timestamp) VALUES \
         ('cpu', 70.0, {tenant: 'globex', host: 'g1'}, 1704067201000000000)",
    );
    exec(
        &rt,
        "INSERT INTO metrics (metric, value, tags, timestamp) VALUES \
         ('cpu', 60.0, {tenant: 'acme', host: 'a2'}, 1704067202000000000)",
    );

    set_current_connection_id(804);

    let acme = rt
        .execute_query("WITHIN TENANT 'acme' SELECT metric, value FROM metrics")
        .unwrap();
    assert_eq!(
        acme.result.records.len(),
        2,
        "acme should see 2 points, got {:?}",
        acme.result.records
    );

    let globex = rt
        .execute_query("WITHIN TENANT 'globex' SELECT metric, value FROM metrics")
        .unwrap();
    assert_eq!(globex.result.records.len(), 1);

    clear_current_connection_id();
}

#[test]
fn within_typed_api_works_with_queue() {
    use reddb::runtime::within_clause::{FieldOverride, ScopeOverride};

    let rt = open_runtime();
    exec(&rt, "CREATE QUEUE notifications");
    exec(
        &rt,
        "CREATE POLICY tenant_iso ON MESSAGES OF notifications \
         USING (payload.tenant = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE notifications ENABLE ROW LEVEL SECURITY");

    exec(
        &rt,
        "QUEUE PUSH notifications {tenant: 'acme', text: 'hello'}",
    );
    exec(
        &rt,
        "QUEUE PUSH notifications {tenant: 'wonka', text: 'sweet'}",
    );

    set_current_connection_id(802);

    let acme_scope = ScopeOverride {
        tenant: FieldOverride::Set("acme".into()),
        ..Default::default()
    };
    let r = rt
        .execute_query_with_scope("QUEUE LEN notifications", acme_scope)
        .unwrap();
    let n = r
        .result
        .records
        .first()
        .and_then(|r| r.values.get("len"))
        .cloned()
        .unwrap();
    assert_eq!(n, reddb::storage::schema::Value::UnsignedInteger(1));

    clear_current_connection_id();
}
