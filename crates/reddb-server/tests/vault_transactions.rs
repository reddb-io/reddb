use std::path::Path;
use std::sync::{Arc, Barrier};

use reddb_server::auth::{AuthConfig, AuthStore};
use reddb_server::runtime::mvcc::{
    clear_current_auth_identity, clear_current_connection_id, clear_current_tenant,
    set_current_connection_id, set_current_tenant,
};
use reddb_server::storage::wal::{WalReader, WalRecord};
use reddb_server::storage::{DeployProfile, StoragePackaging, StorageProfileSelection};
use reddb_server::{RedDBOptions, RedDBRuntime, RuntimeQueryResult, StorageDeployPreset};
use reddb_types::Value;

const CERTIFICATE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

struct Scope;

impl Scope {
    fn new() -> Self {
        clear_current_connection_id();
        clear_current_auth_identity();
        clear_current_tenant();
        Self
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        clear_current_connection_id();
        clear_current_auth_identity();
        clear_current_tenant();
    }
}

fn open(path: &Path, operational: bool) -> RedDBRuntime {
    open_with_durability(
        path,
        operational,
        reddb_server::api::DurabilityMode::WalDurableGrouped,
    )
}

fn open_with_durability(
    path: &Path,
    operational: bool,
    durability: reddb_server::api::DurabilityMode,
) -> RedDBRuntime {
    let profile = if operational {
        StorageProfileSelection {
            deploy_profile: DeployProfile::Embedded,
            packaging: StoragePackaging::OperationalDirectory,
            replica_count: 0,
            managed_backup: false,
            wal_retention: false,
        }
    } else {
        StorageDeployPreset::Serverless.selection()
    };
    let options = RedDBOptions::persistent(path)
        .with_durability_mode(durability)
        .with_storage_profile(profile)
        .expect("persistent profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    let pager = runtime.db().store().pager().cloned().expect("pager");
    let auth = Arc::new(
        AuthStore::with_vault_certificate(AuthConfig::default(), pager, CERTIFICATE)
            .expect("vault"),
    );
    auth.ensure_vault_secret_key();
    runtime.set_auth_store(auth);
    runtime
}

fn execute(runtime: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    runtime
        .execute_query(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

fn reveal(runtime: &RedDBRuntime, path: &str) -> Value {
    execute(runtime, &format!("VAULT REVEAL {path}"))
        .result
        .records[0]
        .get("value")
        .expect("revealed value")
        .clone()
}

fn history_length(runtime: &RedDBRuntime, path: &str) -> usize {
    execute(runtime, &format!("VAULT HISTORY {path}"))
        .result
        .len()
}

fn batches(path: &Path) -> Vec<Vec<Vec<u8>>> {
    WalReader::open(reddb_file::layout::unified_wal_path(path))
        .expect("WAL")
        .iter()
        .filter_map(|record| match record.expect("WAL record").1 {
            WalRecord::TxCommitBatch { actions, .. } => Some(actions),
            _ => None,
        })
        .collect()
}

#[test]
fn secret_commit_reads_own_writes_and_publishes_only_after_commit() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let runtime = open(&directory.path().join("commit.rdb"), false);
    execute(&runtime, "SET SECRET token = 'original'");
    execute(&runtime, "CREATE TABLE probes (id INT, token TEXT)");
    execute(
        &runtime,
        "INSERT INTO probes (id, token) VALUES (1, 'changed'), (2, 'original')",
    );
    let before_events = runtime
        .vault_watch_events_since("red.vault", "token", 0, 100)
        .len();
    set_current_connection_id(11);
    execute(&runtime, "BEGIN");
    execute(&runtime, "SET SECRET token = 'changed'");
    assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("changed"));
    assert_eq!(history_length(&runtime, "red.vault.token"), 2);
    let masked = execute(&runtime, "SELECT LENGTH($secrets.default.token) AS value");
    assert!(matches!(
        masked.result.records[0].get("value"),
        Some(Value::Secret(_))
    ));
    assert_eq!(
        masked.result.records[0]
            .get("value")
            .expect("value")
            .display_string(),
        "***"
    );
    let filtered = execute(
        &runtime,
        "SELECT id FROM probes WHERE token = $secrets.default.token",
    );
    assert_eq!(filtered.result.len(), 1);
    assert_eq!(
        filtered.result.records[0].get("id"),
        Some(&Value::Integer(1))
    );
    assert_eq!(
        runtime
            .vault_watch_events_since("red.vault", "token", 0, 100)
            .len(),
        before_events
    );
    set_current_connection_id(12);
    assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("original"));
    assert_eq!(history_length(&runtime, "red.vault.token"), 1);
    set_current_connection_id(11);
    execute(&runtime, "COMMIT");
    set_current_connection_id(12);
    assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("changed"));
    assert_eq!(
        runtime
            .vault_watch_events_since("red.vault", "token", 0, 100)
            .len(),
        before_events + 1
    );
}

#[test]
fn secret_rollback_discards_put_rotate_delete_and_history() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let runtime = open(&directory.path().join("rollback.rdb"), false);
    execute(&runtime, "CREATE VAULT app");
    execute(&runtime, "VAULT PUT app.token = 'original' TAGS ['base']");
    let before_events = runtime
        .vault_watch_events_since("app", "token", 0, 100)
        .len();
    set_current_connection_id(21);
    execute(&runtime, "BEGIN");
    execute(
        &runtime,
        "VAULT ROTATE app.token = 'rotated' TAGS ['pending']",
    );
    assert_eq!(reveal(&runtime, "app.token"), Value::text("rotated"));
    execute(&runtime, "VAULT DELETE app.token");
    assert!(runtime.execute_query("VAULT REVEAL app.token").is_err());
    execute(&runtime, "VAULT PUT app.token = 'recreated'");
    execute(&runtime, "SET SECRET new.token = 'never-committed'");
    assert_eq!(history_length(&runtime, "app.token"), 4);
    execute(&runtime, "ROLLBACK");
    assert_eq!(reveal(&runtime, "app.token"), Value::text("original"));
    assert_eq!(history_length(&runtime, "app.token"), 1);
    assert_eq!(
        execute(&runtime, "SELECT $secrets.default.new.token AS value")
            .result
            .records[0]
            .get("value"),
        Some(&Value::Null)
    );
    assert_eq!(
        runtime
            .vault_watch_events_since("app", "token", 0, 100)
            .len(),
        before_events
    );
}

#[test]
fn vault_savepoint_rollback_discards_versions_and_watch_events_including_released_children() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let runtime = open(&directory.path().join("savepoints.rdb"), false);
    execute(&runtime, "SET SECRET token = 'original'");
    let before_events = runtime
        .vault_watch_events_since("red.vault", "token", 0, 100)
        .len();
    set_current_connection_id(31);
    execute(&runtime, "BEGIN");
    execute(&runtime, "SET SECRET token = 'outer'");
    execute(&runtime, "SAVEPOINT outer_scope");
    execute(&runtime, "SET SECRET token = 'discarded'");
    execute(&runtime, "SAVEPOINT inner_scope");
    execute(&runtime, "DELETE SECRET token");
    execute(&runtime, "RELEASE SAVEPOINT inner_scope");
    execute(&runtime, "ROLLBACK TO SAVEPOINT outer_scope");
    assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("outer"));
    assert_eq!(history_length(&runtime, "red.vault.token"), 2);
    execute(&runtime, "SAVEPOINT keep");
    execute(&runtime, "SET SECRET token = 'kept'");
    execute(&runtime, "RELEASE SAVEPOINT keep");
    execute(&runtime, "COMMIT");
    assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("kept"));
    assert_eq!(history_length(&runtime, "red.vault.token"), 3);
    assert_eq!(
        runtime
            .vault_watch_events_since("red.vault", "token", 0, 100)
            .len(),
        before_events + 2
    );
}

#[test]
fn vault_reads_observe_snapshot_and_read_committed_isolation() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let runtime = open(&directory.path().join("isolation.rdb"), false);
    execute(&runtime, "SET SECRET token = 'original'");
    for (begin, expected) in [
        ("BEGIN", "original"),
        ("BEGIN ISOLATION LEVEL READ COMMITTED", "changed"),
    ] {
        set_current_connection_id(40);
        execute(&runtime, begin);
        assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("original"));
        set_current_connection_id(41);
        execute(&runtime, "SET SECRET token = 'changed'");
        set_current_connection_id(40);
        assert_eq!(reveal(&runtime, "red.vault.token"), Value::text(expected));
        execute(&runtime, "COMMIT");
        assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("changed"));
        execute(&runtime, "SET SECRET token = 'original'");
    }
}

#[test]
fn concurrent_vault_commits_are_first_committer_wins_for_existing_and_absent_keys() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let runtime = open(&directory.path().join("conflicts.rdb"), false);
    execute(&runtime, "SET SECRET existing = 'original'");
    for key in ["existing", "absent"] {
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = [51, 52]
            .into_iter()
            .map(|connection| {
                let runtime = runtime.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let _scope = Scope::new();
                    set_current_connection_id(connection);
                    execute(&runtime, "BEGIN");
                    execute(
                        &runtime,
                        &format!("SET SECRET {key} = 'writer-{connection}'"),
                    );
                    barrier.wait();
                    (connection, runtime.execute_query("COMMIT"))
                })
            })
            .collect();
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("writer"))
            .collect();
        assert_eq!(
            outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
        let winner = outcomes
            .iter()
            .find(|(_, result)| result.is_ok())
            .expect("winner")
            .0;
        let loser = outcomes
            .iter()
            .find(|(_, result)| result.is_err())
            .expect("conflict")
            .1
            .as_ref()
            .expect_err("conflict");
        assert!(loser.to_string().contains("serialization conflict"));
        assert_eq!(
            reveal(&runtime, &format!("red.vault.{key}")),
            Value::text(format!("writer-{winner}"))
        );
        assert_eq!(
            history_length(&runtime, &format!("red.vault.{key}")),
            if key == "existing" { 2 } else { 1 }
        );
    }
}

#[test]
fn autocommit_conflict_aborts_all_staged_vault_and_table_writes() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    for operational in [false, true] {
        let path = directory
            .path()
            .join(format!("mixed-conflict-{operational}.rdb"));
        let runtime = open(&path, operational);
        execute(&runtime, "CREATE TABLE markers (id INT)");
        execute(&runtime, "SET SECRET token = 'original'");
        set_current_connection_id(61);
        execute(&runtime, "BEGIN ISOLATION LEVEL READ COMMITTED");
        execute(&runtime, "INSERT INTO markers (id) VALUES (61)");
        execute(&runtime, "SET SECRET token = 'loser'");
        execute(&runtime, "SET SECRET other = 'also-loser'");
        set_current_connection_id(62);
        execute(&runtime, "SET SECRET token = 'winner'");
        set_current_connection_id(61);
        assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("loser"));
        assert!(runtime
            .execute_query("COMMIT")
            .expect_err("conflict")
            .to_string()
            .contains("serialization conflict"));
        assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("winner"));
        assert!(runtime
            .execute_query("VAULT REVEAL red.vault.other")
            .is_err());
        assert_eq!(execute(&runtime, "SELECT id FROM markers").result.len(), 0);
        runtime
            .checkpoint()
            .expect("checkpoint aborted mixed transaction");
        drop(runtime);
        clear_current_connection_id();
        let recovered = open(&path, operational);
        assert_eq!(reveal(&recovered, "red.vault.token"), Value::text("winner"));
        assert!(recovered
            .execute_query("VAULT REVEAL red.vault.other")
            .is_err());
        assert_eq!(
            execute(&recovered, "SELECT id FROM markers").result.len(),
            0
        );
    }
}

#[test]
fn tenant_local_vault_write_sets_do_not_conflict_or_cross_scopes() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let runtime = open(&directory.path().join("tenants.rdb"), false);
    for (connection, tenant) in [(71, "acme"), (72, "other")] {
        set_current_connection_id(connection);
        set_current_tenant(tenant.into());
        execute(&runtime, "BEGIN");
        execute(&runtime, &format!("SET SECRET token = '{tenant}-value'"));
    }
    for (connection, tenant) in [(71, "acme"), (72, "other")] {
        set_current_connection_id(connection);
        set_current_tenant(tenant.into());
        assert_eq!(
            reveal(&runtime, "red.vault.token"),
            Value::text(format!("{tenant}-value"))
        );
        execute(&runtime, "COMMIT");
    }
    clear_current_tenant();
    assert!(runtime
        .execute_query("VAULT REVEAL red.vault.token")
        .is_err());
}

#[test]
fn vault_and_table_commit_use_one_wal_batch_and_recover_together() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("atomic.rdb");
    {
        let runtime = open(&path, true);
        execute(&runtime, "CREATE TABLE markers (id INT)");
        execute(&runtime, "SET SECRET token = 'original'");
        let before = batches(&path).len();
        set_current_connection_id(81);
        execute(&runtime, "BEGIN");
        execute(&runtime, "INSERT INTO markers (id) VALUES (81)");
        execute(&runtime, "SET SECRET token = 'committed'");
        assert_eq!(
            batches(&path).len(),
            before,
            "no WAL publication before commit"
        );
        execute(&runtime, "COMMIT");
        let after = batches(&path);
        assert_eq!(after.len(), before + 1);
        let actions = after.last().expect("commit batch");
        for collection in ["markers", "red.vault"] {
            assert!(actions.iter().any(|action| action
                .windows(collection.len())
                .any(|bytes| bytes == collection.as_bytes())));
        }
        assert!(actions.iter().all(|action| !action
            .windows("committed".len())
            .any(|bytes| bytes == b"committed")));
    }
    clear_current_connection_id();
    let recovered = open(&path, true);
    assert_eq!(
        reveal(&recovered, "red.vault.token"),
        Value::text("committed")
    );
    assert_eq!(
        execute(&recovered, "SELECT id FROM markers").result.len(),
        1
    );
}

#[test]
fn checkpoint_and_backup_exclude_pending_vault_versions_after_runtime_drop() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    for operational in [false, true] {
        let path = directory
            .path()
            .join(format!("checkpoint-{operational}.rdb"));
        {
            let runtime = open(&path, operational);
            execute(&runtime, "SET SECRET token = 'original'");
            let before = runtime
                .export_vault_collection_records("red.vault")
                .expect("dump");
            set_current_connection_id(91);
            execute(&runtime, "BEGIN");
            execute(&runtime, "SET SECRET token = 'abandoned'");
            execute(&runtime, "SET SECRET uncommitted = 'abandoned-new-key'");
            assert_eq!(
                runtime
                    .export_vault_collection_records("red.vault")
                    .expect("pending dump"),
                before
            );
            runtime
                .checkpoint()
                .expect("checkpoint while transaction remains open");
            // Drop without COMMIT; a separate test exercises actual pooled lease reuse.
        }
        clear_current_connection_id();
        let recovered = open(&path, operational);
        assert_eq!(
            reveal(&recovered, "red.vault.token"),
            Value::text("original")
        );
        assert!(recovered
            .execute_query("VAULT REVEAL red.vault.uncommitted")
            .is_err());
        assert_eq!(history_length(&recovered, "red.vault.token"), 1);
    }
}

#[test]
fn returning_a_connection_to_the_pool_aborts_its_vault_transaction() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let runtime = open(&directory.path().join("disconnect.rdb"), false);
    execute(&runtime, "SET SECRET token = 'original'");
    let before_events = runtime
        .vault_watch_events_since("red.vault", "token", 0, 100)
        .len();
    let connection = runtime.acquire().expect("lease");
    let id = connection.id();
    set_current_connection_id(id);
    execute(&runtime, "BEGIN");
    execute(&runtime, "SET SECRET token = 'abandoned'");
    execute(&runtime, "SET SECRET uncommitted = 'abandoned-new-key'");
    // A lease may be dropped on another thread or while another session is current.
    set_current_connection_id(999);
    execute(&runtime, "BEGIN");
    execute(&runtime, "SET SECRET survivor = 'other-session'");
    drop(connection);
    assert_eq!(
        reveal(&runtime, "red.vault.survivor"),
        Value::text("other-session")
    );
    execute(&runtime, "COMMIT");
    let next = runtime.acquire().expect("reused lease");
    assert_eq!(next.id(), id);
    set_current_connection_id(next.id());
    assert_eq!(reveal(&runtime, "red.vault.token"), Value::text("original"));
    assert!(runtime
        .execute_query("VAULT REVEAL red.vault.uncommitted")
        .is_err());
    execute(&runtime, "COMMIT");
    assert_eq!(history_length(&runtime, "red.vault.token"), 1);
    assert_eq!(
        runtime
            .vault_watch_events_since("red.vault", "token", 0, 100)
            .len(),
        before_events
    );
    execute(&runtime, "BEGIN");
    execute(&runtime, "SET SECRET token = 'fresh-session'");
    execute(&runtime, "COMMIT");
    assert_eq!(
        reveal(&runtime, "red.vault.token"),
        Value::text("fresh-session")
    );
}

#[test]
fn serializable_vault_absence_and_listing_reads_prevent_write_skew() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    for listing in [false, true] {
        let runtime = open(
            &directory.path().join(format!("serializable-{listing}.rdb")),
            false,
        );
        // Provision the empty default vault before either transaction takes a snapshot.
        execute(&runtime, "SET SECRET bootstrap = 'initial'");
        for (connection, missing, written) in [(101, "beta", "alpha"), (102, "alpha", "beta")] {
            set_current_connection_id(connection);
            execute(&runtime, "BEGIN ISOLATION LEVEL SERIALIZABLE");
            if listing {
                execute(&runtime, "SHOW SECRETS");
            } else {
                assert_eq!(
                    execute(
                        &runtime,
                        &format!("SELECT $secrets.default.{missing} AS value")
                    )
                    .result
                    .records[0]
                        .get("value"),
                    Some(&Value::Null)
                );
            }
            execute(&runtime, &format!("SET SECRET {written} = 'created'"));
        }
        set_current_connection_id(101);
        execute(&runtime, "COMMIT");
        set_current_connection_id(102);
        assert!(runtime
            .execute_query("COMMIT")
            .expect_err("write skew must abort")
            .to_string()
            .contains("serialization conflict"));
        assert!(runtime
            .execute_query("VAULT REVEAL red.vault.beta")
            .is_err());
        assert_eq!(reveal(&runtime, "red.vault.alpha"), Value::text("created"));
    }
}

#[test]
fn committed_vault_tombstones_survive_checkpoint_and_restart() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    for operational in [false, true] {
        let path = directory.path().join(format!("deleted-{operational}.rdb"));
        {
            let runtime = open(&path, operational);
            execute(&runtime, "SET SECRET token = 'original'");
            set_current_connection_id(111);
            execute(&runtime, "BEGIN");
            execute(&runtime, "DELETE SECRET token");
            execute(&runtime, "COMMIT");
            runtime.checkpoint().expect("checkpoint");
        }
        clear_current_connection_id();
        let recovered = open(&path, operational);
        assert!(recovered
            .execute_query("VAULT REVEAL red.vault.token")
            .is_err());
        assert_eq!(history_length(&recovered, "red.vault.token"), 2);
        execute(&recovered, "SET SECRET token = 'recreated'");
        assert_eq!(history_length(&recovered, "red.vault.token"), 3);
    }
}

#[test]
fn strict_vault_commit_is_wal_durable_before_checkpoint() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("strict.rdb");
    let runtime = open_with_durability(&path, true, reddb_server::api::DurabilityMode::Strict);
    execute(&runtime, "SET SECRET token = 'original'");
    let before = batches(&path).len();
    set_current_connection_id(131);
    execute(&runtime, "BEGIN");
    execute(&runtime, "SET SECRET token = 'committed'");
    execute(&runtime, "COMMIT");
    assert_eq!(batches(&path).len(), before + 1);
    assert_eq!(
        reveal(&runtime, "red.vault.token"),
        Value::text("committed")
    );
    runtime.checkpoint().expect("checkpoint");
    drop(runtime);
    clear_current_connection_id();
    let recovered = open_with_durability(&path, true, reddb_server::api::DurabilityMode::Strict);
    assert_eq!(
        reveal(&recovered, "red.vault.token"),
        Value::text("committed")
    );
}

#[test]
fn strict_replay_preserves_autocommit_changes_after_transaction_commit() {
    let _scope = Scope::new();
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("strict-replay.rdb");
    {
        let runtime = open_with_durability(&path, true, reddb_server::api::DurabilityMode::Strict);
        execute(&runtime, "CREATE TABLE markers (id INT, token TEXT)");
        set_current_connection_id(141);
        execute(&runtime, "BEGIN");
        execute(
            &runtime,
            "INSERT INTO markers (id, token) VALUES (1, 'first'), (2, 'deleted')",
        );
        execute(&runtime, "SET SECRET token = 'committed'");
        execute(&runtime, "COMMIT");
        execute(&runtime, "UPDATE markers SET token = 'latest' WHERE id = 1");
        execute(&runtime, "DELETE FROM markers WHERE id = 2");
    }
    clear_current_connection_id();
    let recovered = open_with_durability(&path, true, reddb_server::api::DurabilityMode::Strict);
    let rows = execute(&recovered, "SELECT token FROM markers");
    assert_eq!(rows.result.len(), 1);
    assert_eq!(
        rows.result.records[0].get("token"),
        Some(&Value::text("latest"))
    );
    assert_eq!(
        reveal(&recovered, "red.vault.token"),
        Value::text("committed")
    );
}
