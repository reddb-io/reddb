//! Regression: in the Embedded + SingleFile storage profile (the default for
//! `RedDBOptions::persistent`), a collection's declared model must survive a
//! restart. The contracts that carry `declared_model` live only in RedDB's
//! in-memory cache; before the fix they were never persisted, so on reopen the
//! catalog re-inferred the model from the stored entities. A KV collection is
//! physically stored as table rows, so it came back as a `Table` instead of
//! `Kv` — the bug reported against 1.12.0.

use reddb_server::catalog::CollectionModel;
use reddb_server::{RedDBOptions, RedDBRuntime};

fn declared_model(rt: &RedDBRuntime, collection: &str) -> Option<CollectionModel> {
    rt.db()
        .collection_contract(collection)
        .map(|contract| contract.declared_model)
}

#[test]
fn kv_collection_model_survives_single_file_restart() {
    let dir = tempfile::Builder::new()
        .prefix("reddb-kv-model-recovery-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("data.rdb");

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("runtime boots persistent (single-file)");
        rt.execute_query("CREATE KV sessions")
            .expect("create kv collection");
        // Store a value so the collection holds table-row entities — exactly
        // the shape that made recovery re-infer the model as a table.
        rt.execute_query("KV PUT sessions.token = 'abc123'")
            .expect("kv put");

        assert_eq!(
            declared_model(&rt, "sessions"),
            Some(CollectionModel::Kv),
            "model should be Kv before restart"
        );
        // Drop closes the runtime and flushes the single-file artifact.
    }

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("runtime reopens persistent (single-file)");
        assert_eq!(
            declared_model(&rt, "sessions"),
            Some(CollectionModel::Kv),
            "KV collection must not come back as a table after a restart"
        );
    }
}
