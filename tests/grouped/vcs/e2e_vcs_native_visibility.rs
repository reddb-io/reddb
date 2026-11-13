//! The native entity API must use the same visibility as SQL for each
//! entity-backed collection, including its page totals and stable IDs.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::RedDBRuntime;

fn check_pending_insert(create: &str, insert: &str) {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(8101);
    runtime.execute_query(create).expect("create collection");
    runtime.vcs_set_versioned("items", true).expect("versioned");
    runtime.execute_query("BEGIN").expect("begin");
    runtime.execute_query(insert).expect("insert");
    let own_page = runtime.scan_collection("items", None, 1).expect("own scan");
    assert_eq!(own_page.total, 1);
    assert_eq!(own_page.items.len(), 1);
    let id = own_page.items[0].logical_id();
    assert!(runtime
        .get_collection_entity("items", id)
        .expect("own get")
        .is_some());

    set_current_connection_id(8102);
    let peer_page = runtime
        .scan_collection("items", None, 1)
        .expect("peer scan");
    assert_eq!(peer_page.total, 0, "pending writes must not affect totals");
    assert!(peer_page.items.is_empty());
    assert!(peer_page.next.is_none());
    assert!(runtime
        .get_collection_entity("items", id)
        .expect("peer get")
        .is_none());

    set_current_connection_id(8101);
    runtime.execute_query("COMMIT").expect("commit");
    set_current_connection_id(8102);
    assert_eq!(
        runtime
            .scan_collection("items", None, 1)
            .expect("committed scan")
            .total,
        1
    );
    assert!(runtime
        .get_collection_entity("items", id)
        .expect("committed get")
        .is_some());
    clear_current_connection_id();
}

#[test]
fn table_native_reads_hide_pending_inserts() {
    check_pending_insert(
        "CREATE TABLE items (name TEXT)",
        "INSERT INTO items (name) VALUES ('pending')",
    );
}

#[test]
fn document_native_reads_hide_pending_inserts() {
    check_pending_insert(
        "CREATE DOCUMENT items",
        "INSERT INTO items DOCUMENT VALUES ({name:'pending'})",
    );
}

#[test]
fn graph_native_reads_hide_pending_inserts() {
    check_pending_insert(
        "CREATE GRAPH items",
        "INSERT INTO items NODE (label, node_type) VALUES ('pending', 'Person')",
    );
}

#[test]
fn vector_native_reads_hide_pending_inserts() {
    check_pending_insert(
        "CREATE VECTOR items DIM 2 METRIC cosine",
        "INSERT INTO items VECTOR (dense, content) VALUES ([1.0, 0.0], 'pending')",
    );
}

#[test]
fn kv_native_reads_hide_pending_inserts() {
    check_pending_insert("CREATE KV items", "KV PUT items.'pending' = 'value'");
}

#[test]
fn native_snapshot_filters_tenant_rows_before_pagination() {
    use reddb::runtime::mvcc::{
        clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
        set_current_tenant,
    };
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(8103);
    runtime
        .execute_query("CREATE TABLE items (name TEXT, tenant_id TEXT) TENANT BY (tenant_id)")
        .expect("tenant table");
    runtime.vcs_set_versioned("items", true).expect("versioned");
    runtime.execute_query(
        "INSERT INTO items (name, tenant_id) VALUES ('first','acme'),('hidden','globex'),('last','acme')",
    ).expect("committed rows");
    let all = runtime
        .scan_collection("items", None, 10)
        .expect("platform scan");
    let foreign = all
        .items
        .iter()
        .find(|entity| {
            entity.data.as_row().and_then(|row| row.get_field("name"))
                == Some(&reddb_types::Value::text("hidden"))
        })
        .expect("foreign row")
        .logical_id();
    runtime.execute_query("BEGIN").expect("begin writer");
    runtime
        .execute_query("INSERT INTO items (name, tenant_id) VALUES ('pending','acme')")
        .expect("pending row");

    set_current_connection_id(8104);
    set_current_tenant("acme".into());
    set_current_auth_identity("alice".into(), reddb::auth::Role::Read);
    let first = runtime
        .scan_collection("items", None, 1)
        .expect("first page");
    assert_eq!(first.total, 2);
    assert_eq!(first.items.len(), 1);
    let next = first.next.expect("next visible page");
    let last = runtime
        .scan_collection("items", Some(next), 1)
        .expect("last page");
    assert_eq!(last.total, 2);
    assert_eq!(last.items.len(), 1);
    assert!(last.next.is_none());
    assert!(runtime
        .get_collection_entity("items", foreign)
        .expect("foreign get")
        .is_none());
    clear_current_auth_identity();
    clear_current_tenant();
    set_current_connection_id(8103);
    runtime.execute_query("ROLLBACK").expect("rollback writer");
    clear_current_connection_id();
}

#[test]
fn native_snapshot_applies_graph_and_document_policies() {
    use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    for query in [
        "CREATE GRAPH links",
        "INSERT INTO links NODE (label,node_type,visible) VALUES ('shown','Item',true),('hidden','Item',false)",
        "CREATE POLICY visible_nodes ON NODES OF links FOR SELECT USING (properties.visible=true)",
        "ALTER TABLE links ENABLE ROW LEVEL SECURITY",
        "CREATE DOCUMENT papers",
        "INSERT INTO papers DOCUMENT VALUES ({name:'shown',visible:true}),({name:'hidden',visible:false})",
        "CREATE POLICY visible_documents ON DOCUMENTS OF papers FOR SELECT USING (visible=true)",
        "ALTER TABLE papers ENABLE ROW LEVEL SECURITY",
    ] {
        runtime.execute_query(query).expect(query);
    }
    let ids = ["links", "papers"].map(|collection| {
        runtime
            .scan_collection(collection, None, 10)
            .expect("platform rows")
            .items
            .into_iter()
            .map(|entity| entity.logical_id())
            .collect::<Vec<_>>()
    });
    set_current_auth_identity("alice".into(), reddb::auth::Role::Read);
    for (collection, ids) in ["links", "papers"].into_iter().zip(ids) {
        let page = runtime
            .scan_collection(collection, None, 1)
            .expect("policy page");
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 1);
        assert!(page.next.is_none());
        let visible = ids
            .into_iter()
            .filter(|id| {
                runtime
                    .get_collection_entity(collection, *id)
                    .expect("policy get")
                    .is_some()
            })
            .count();
        assert_eq!(visible, 1);
    }
    clear_current_auth_identity();
}
