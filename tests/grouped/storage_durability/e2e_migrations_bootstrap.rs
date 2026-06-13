//! Bootstrap tests for native migrations (issue #8).
//!
//! Verifies that `red_migrations` and `red_migration_deps` exist after
//! engine startup and that querying them returns zero rows on a fresh
//! in-memory instance.

use reddb::application::vcs::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput,
};
use reddb::application::VcsUseCases;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn text(value: Option<&Value>) -> &str {
    match value {
        Some(Value::Text(s)) => s.as_ref(),
        other => panic!("expected text value, got {other:?}"),
    }
}

fn author() -> Author {
    Author {
        name: "test".to_string(),
        email: "test@reddb.io".to_string(),
    }
}

fn commit_input(conn: u64, msg: &str) -> CreateCommitInput {
    CreateCommitInput {
        connection_id: conn,
        message: msg.to_string(),
        author: author(),
        committer: None,
        amend: false,
        allow_empty: true,
    }
}

fn vcs(rt: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(rt)
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

#[test]
fn migration_registration_is_global_across_vcs_branches() {
    let rt = rt();
    vcs(&rt)
        .commit(commit_input(1, "root"))
        .expect("root commit");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "feature-migration".to_string(),
            from: None,
            connection_id: 1,
        })
        .expect("create feature branch");
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("feature-migration".to_string()),
            force: false,
        })
        .expect("checkout feature branch");

    rt.execute_query(
        "CREATE MIGRATION branch_visible AS \
         CREATE TABLE branch_visible_accounts (id BIGINT)",
    )
    .expect("register migration on feature branch");

    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .expect("checkout main");

    let migrations = rt
        .execute_query("SELECT name FROM red_migrations")
        .expect("list migrations from main");
    let names: Vec<&str> = migrations
        .result
        .records
        .iter()
        .map(|record| text(record.get("name")))
        .collect();
    assert!(
        names.contains(&"branch_visible"),
        "migration definitions are currently global across branches: {names:?}"
    );
}

#[test]
fn apply_migration_all_applies_pending_migrations_in_dependency_order() {
    let rt = rt();

    rt.execute_query(
        "CREATE MIGRATION create_accounts AS \
         CREATE TABLE mig_accounts (id BIGINT, name TEXT)",
    )
    .expect("create_accounts migration");
    rt.execute_query(
        "CREATE MIGRATION seed_accounts \
         DEPENDS ON create_accounts AS \
         INSERT INTO mig_accounts (id, name) VALUES (1, 'Ada')",
    )
    .expect("seed_accounts migration");
    rt.execute_query(
        "CREATE MIGRATION create_audit AS \
         CREATE TABLE mig_audit (id BIGINT, event TEXT)",
    )
    .expect("create_audit migration");
    rt.execute_query(
        "CREATE MIGRATION seed_audit \
         DEPENDS ON create_audit AS \
         INSERT INTO mig_audit (id, event) VALUES (1, 'created')",
    )
    .expect("seed_audit migration");
    rt.execute_query(
        "CREATE MIGRATION seed_accounts_second \
         DEPENDS ON seed_accounts AS \
         INSERT INTO mig_accounts (id, name) VALUES (2, 'Grace')",
    )
    .expect("seed_accounts_second migration");

    let applied = rt
        .execute_query("APPLY MIGRATION *")
        .expect("bulk apply migrations");
    let message = text(applied.result.records[0].get("message"));
    assert!(
        message.contains("applied 5 migration(s)"),
        "unexpected bulk apply message: {message}"
    );

    let migrations = rt
        .execute_query("SELECT name, status, vcs_commit_hash FROM red_migrations")
        .expect("list migrations");
    assert_eq!(migrations.result.records.len(), 5);
    for record in &migrations.result.records {
        assert_eq!(text(record.get("status")), "applied");
        assert!(
            !text(record.get("vcs_commit_hash")).is_empty(),
            "applied migration should have a VCS commit hash: {record:?}"
        );
    }

    let accounts = rt
        .execute_query("SELECT * FROM mig_accounts")
        .expect("accounts visible after dependency-ordered apply");
    assert_eq!(accounts.result.records.len(), 2);

    let audit = rt
        .execute_query("SELECT * FROM mig_audit")
        .expect("audit visible after independent migration apply");
    assert_eq!(audit.result.records.len(), 1);
}
