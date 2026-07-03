use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore};
use reddb::{
    storage::{DeployProfile, StoragePackaging, StorageProfileSelection},
    RedDBOptions, RedDBRuntime,
};

#[path = "support/mod.rs"]
mod support;

const TEST_CERTIFICATE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

const QUICKSTARTS: &[&str] = &[
    "docs/getting-started/relational-sql.md",
    "docs/getting-started/documents.md",
    "docs/getting-started/key-value-config-vault.md",
    "docs/getting-started/graph.md",
    "docs/getting-started/vector-search.md",
    "docs/getting-started/timeseries.md",
    "docs/getting-started/queues.md",
    "docs/getting-started/spatial.md",
    "docs/getting-started/vcs.md",
    "docs/getting-started/ask-rag.md",
];

#[test]
fn getting_started_quickstarts_execute() {
    for path in QUICKSTARTS {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("read quickstart {path}: {err}"));
        let blocks = executable_sql_blocks(&source);
        assert!(!blocks.is_empty(), "{path} has no executable SQL blocks");

        let (_guard, rt) = runtime_for(path);

        for block in blocks {
            for statement in statements(&block) {
                rt.execute_query(statement)
                    .unwrap_or_else(|err| panic!("{path} failed on {statement:?}: {err}"));
            }
        }
    }
}

fn runtime_for(path: &str) -> (Option<support::TempDbFile>, RedDBRuntime) {
    if path.ends_with("key-value-config-vault.md") {
        let db = support::temp_db_file("docs-quickstart-vault");
        let options = RedDBOptions::persistent(db.path())
            .with_storage_profile(StorageProfileSelection {
                deploy_profile: DeployProfile::Embedded,
                packaging: StoragePackaging::OperationalDirectory,
                replica_count: 0,
                managed_backup: false,
                wal_retention: false,
            })
            .expect("operational storage profile should validate");
        let rt = RedDBRuntime::with_options(options).expect("open vault quickstart runtime");
        let pager = Arc::clone(
            rt.db()
                .store()
                .pager()
                .expect("persistent runtime should expose pager"),
        );
        let auth = Arc::new(
            AuthStore::with_vault_certificate(AuthConfig::default(), pager, TEST_CERTIFICATE)
                .expect("vault should open"),
        );
        rt.set_auth_store(auth);
        return (Some(db), rt);
    }

    (
        None,
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("open in-memory runtime"),
    )
}

fn executable_sql_blocks(source: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = None;

    for line in source.lines() {
        if line.trim() == "```sql quickstart" {
            current = Some(Vec::new());
            continue;
        }

        if line.trim() == "```" {
            if let Some(lines) = current.take() {
                blocks.push(lines.join("\n"));
            }
            continue;
        }

        if let Some(lines) = current.as_mut() {
            lines.push(line);
        }
    }

    blocks
}

fn statements(block: &str) -> impl Iterator<Item = &str> {
    block
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
}
