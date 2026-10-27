//! Exercise the persistent insertion path with the same indexed row shape as the wire repro.
use reddb_server::{RedDBError, RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[test]
fn persistent_context_growth_returns_a_budget_error_before_publication() {
    let directory = tempfile::tempdir().expect("temporary store");
    let path = directory.path().join("context.rdb");
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path)
            .with_memory_budget(8 * 1024 * 1024)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile"),
    )
    .expect("runtime");
    let columns = (0..12)
        .map(|f| format!("f{f} TEXT"))
        .collect::<Vec<_>>()
        .join(",");
    runtime
        .execute_query(&format!(
            "CREATE TABLE probe (id INTEGER PRIMARY KEY,{columns}) WITH context_index = true"
        ))
        .expect("table");
    let columns = (0..12)
        .map(|f| format!("f{f}"))
        .collect::<Vec<_>>()
        .join(",");
    for first in (0..2000).step_by(10) {
        let values = (first..first + 10)
            .map(|id| {
                let fields = (0..12)
                    .map(|f| format!("'{id:08x}-{f:02}-{}'", "x".repeat(20)))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("({id},{fields})")
            })
            .collect::<Vec<_>>()
            .join(",");
        if let Err(error) =
            runtime.execute_query(&format!("INSERT INTO probe (id,{columns}) VALUES {values}"))
        {
            eprintln!("first_rejected_id={first}, error={error:?}");
            assert!(matches!(&error, RedDBError::InvalidOperation(message) if message.contains("over budget")),
                "reject through shared admission, not a partially published storage failure: {error:?}");
            assert!(reddb_server::api::client_facing_message(&error).contains("over budget"));
            let rows = runtime
                .execute_query("SELECT id FROM probe ORDER BY id")
                .expect("accepted rows")
                .result
                .records;
            assert_eq!(rows.len(), first);
            runtime.refresh_memory_accounting();
            assert!(runtime.memory_accounting().total_used_bytes() <= 8 * 1024 * 1024);
            return;
        }
    }
    panic!("the reproduction must reach the shared context budget");
}
