use reddb::application::ports::RuntimeEntityPort;
use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, DeleteEntityInput,
    PatchEntityInput,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::RedDBRuntime;

#[test]
fn native_patch_and_delete_resolve_branch_versions_and_preserve_history() {
    let fixtures = [
        (
            "CREATE TABLE items (value INT)",
            "INSERT INTO items (value) VALUES (1)",
            r#"{"fields":{"value":2}}"#,
        ),
        (
            "CREATE DOCUMENT items",
            "INSERT INTO items DOCUMENT VALUES ({value:1})",
            r#"{"body":{"value":2}}"#,
        ),
        (
            "CREATE GRAPH items",
            "INSERT INTO items NODE (label,node_type,score) VALUES ('first','Person',1)",
            r#"{"fields":{"score":2}}"#,
        ),
        (
            "CREATE VECTOR items DIM 2 METRIC cosine",
            "INSERT INTO items VECTOR (dense,content) VALUES ([1.0,0.0],'first')",
            r#"{"fields":{"dense":[0.0,1.0],"content":"second"}}"#,
        ),
        (
            "CREATE KV items",
            "KV PUT items.first = 1",
            r#"{"fields":{"value":2}}"#,
        ),
    ];
    for (create, insert, patch) in fixtures {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        set_current_connection_id(9401);
        runtime.execute_query(create).expect("collection");
        runtime.vcs_set_versioned("items", true).expect("versioned");
        runtime.execute_query(insert).expect("base");
        let base = runtime
            .scan_collection("items", None, 10)
            .expect("scan")
            .items
            .remove(0);
        let id = base.logical_id();
        let checkpoint = runtime
            .vcs_commit(CreateCommitInput {
                connection_id: 9401,
                message: "base".into(),
                author: Author {
                    name: "test".into(),
                    email: "test@reddb.io".into(),
                },
                committer: None,
                amend: false,
                allow_empty: true,
            })
            .expect("checkpoint");
        runtime
            .vcs_branch_create(CreateBranchInput {
                connection_id: 9401,
                name: "agent".into(),
                from: None,
            })
            .expect("fork");
        let checkout = |name: &str| {
            runtime
                .vcs_checkout(CheckoutInput {
                    connection_id: 9401,
                    target: CheckoutTarget::Branch(name.into()),
                    force: false,
                })
                .expect("checkout")
        };
        checkout("agent");
        for _ in 0..2 {
            let result = runtime
                .patch_entity(PatchEntityInput {
                    collection: "items".into(),
                    id,
                    payload: reddb::json::from_str(patch).expect("patch"),
                    operations: Vec::new(),
                })
                .expect("native patch uses stable logical identity");
            assert_eq!(result.id, id);
        }
        assert_ne!(
            runtime
                .get_collection_entity("items", id)
                .expect("updated")
                .expect("row")
                .data,
            base.data
        );
        runtime.execute_query("BEGIN").expect("begin");
        assert!(
            runtime
                .delete_entity(DeleteEntityInput {
                    collection: "items".into(),
                    id
                })
                .expect("delete")
                .deleted
        );
        assert!(runtime
            .get_collection_entity("items", id)
            .expect("own delete")
            .is_none());
        runtime.execute_query("ROLLBACK").expect("rollback");
        assert!(runtime
            .get_collection_entity("items", id)
            .expect("revived")
            .is_some());
        assert!(
            runtime
                .delete_entity(DeleteEntityInput {
                    collection: "items".into(),
                    id
                })
                .expect("delete")
                .deleted
        );
        assert!(
            !runtime
                .delete_entity(DeleteEntityInput {
                    collection: "items".into(),
                    id
                })
                .expect("repeat")
                .deleted
        );
        checkout("main");
        assert_eq!(
            runtime
                .get_collection_entity("items", id)
                .expect("main")
                .expect("row")
                .data,
            base.data
        );
        let history = runtime
            .execute_query(&format!(
                "SELECT * FROM items AS OF COMMIT '{}'",
                checkpoint.hash
            ))
            .expect("retained history");
        assert_eq!(history.result.records.len(), 1);
        clear_current_connection_id();
    }
}
