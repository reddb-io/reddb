//! Bootstrap tests for native migrations (issue #8).
//!
//! Verifies that `red_migrations` and `red_migration_deps` exist after
//! engine startup and that querying them returns zero rows on a fresh
//! in-memory instance.

use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

#[test]
fn red_migrations_exists_and_is_empty() {
    let rt = rt();
    let result = rt
        .execute_query("SELECT * FROM red_migrations")
        .expect("SELECT * FROM red_migrations should not error");
    assert_eq!(
        result.result.records.len(),
        0,
        "red_migrations should be empty on a fresh instance"
    );
}

#[test]
fn red_migration_deps_exists_and_is_empty() {
    let rt = rt();
    let result = rt
        .execute_query("SELECT * FROM red_migration_deps")
        .expect("SELECT * FROM red_migration_deps should not error");
    assert_eq!(
        result.result.records.len(),
        0,
        "red_migration_deps should be empty on a fresh instance"
    );
}
