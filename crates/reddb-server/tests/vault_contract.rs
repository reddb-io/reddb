use std::path::Path;
use std::sync::Arc;

use reddb_server::auth::policies::Policy;
use reddb_server::auth::store::PrincipalRef;
use reddb_server::auth::vault::{Vault, VaultState};
use reddb_server::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb_server::runtime::mvcc::{
    clear_current_auth_identity, clear_current_connection_id, clear_current_tenant,
    set_current_auth_identity, set_current_connection_id, set_current_tenant,
};
use reddb_server::storage::{EntityData, StorageDeployPreset};
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

const CERTIFICATE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("paged storage profile")
}

struct Scope;
impl Drop for Scope {
    fn drop(&mut self) {
        clear_current_auth_identity();
        clear_current_tenant();
        clear_current_connection_id();
    }
}

fn scope(tenant: Option<&str>, username: Option<&str>, role: Role) -> Scope {
    clear_current_auth_identity();
    clear_current_tenant();
    if let Some(tenant) = tenant {
        set_current_tenant(tenant.into());
    }
    if let Some(username) = username {
        set_current_auth_identity(username.into(), role);
    }
    Scope
}

fn open(path: &Path) -> (RedDBRuntime, Arc<AuthStore>) {
    let runtime = RedDBRuntime::with_options(options(path)).expect("runtime");
    let pager = runtime.db().store().pager().cloned().expect("pager");
    let auth = Arc::new(
        AuthStore::with_vault_certificate(AuthConfig::default(), pager, CERTIFICATE)
            .expect("vault"),
    );
    auth.ensure_vault_secret_key();
    runtime.set_auth_store(Arc::clone(&auth));
    (runtime, auth)
}

fn value(runtime: &RedDBRuntime, sql: &str, column: &str) -> Value {
    let result = runtime
        .execute_query(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    result.result.records[0]
        .get(column)
        .expect("column")
        .clone()
}

fn grant(auth: &AuthStore, user: UserId, id: &str, statements: &str) {
    let json = format!(r#"{{"id":"{id}","version":1,"statements":{statements}}}"#);
    auth.put_policy(Policy::from_json_str(&json).expect("policy"))
        .expect("store policy");
    auth.attach_policy(PrincipalRef::User(user), id)
        .expect("attach policy");
}

fn contains_plaintext(path: &Path, probe: &[u8]) -> bool {
    std::fs::read_dir(path).expect("directory").any(|entry| {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            contains_plaintext(&path, probe)
        } else {
            std::fs::read(path)
                .expect("file")
                .windows(probe.len())
                .any(|bytes| bytes == probe)
        }
    })
}

#[test]
fn window_outputs_remain_secret_when_the_destination_row_has_null_or_public_input() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, _) = open(&directory.path().join("window-null.rdb"));
    runtime
        .execute_query("CREATE TABLE typed_windows (id INTEGER, token SECRET)")
        .expect("typed windows");
    runtime
        .db()
        .store()
        .create_collection("mixed_windows")
        .expect("untyped windows");
    for table in ["typed_windows", "mixed_windows"] {
        runtime
            .execute_query(&format!(
                "INSERT INTO {table} (id, token) VALUES (1, SECRET('synthetic-window-value')), (2, NULL), (3, 'public')"
            ))
            .expect("window rows");
        for expression in [
            "LAG(token) OVER (ORDER BY id)",
            "LEAD(token) OVER (ORDER BY id)",
            "MIN(token) OVER ()",
            "MAX(token) OVER ()",
            "LAG(CASE WHEN id = 1 THEN token ELSE 'public' END) OVER (ORDER BY id)",
        ] {
            let result = runtime
                .execute_query(&format!(
                    "SELECT id, {expression} AS value FROM {table} ORDER BY id"
                ))
                .expect("window projection");
            assert_eq!(result.result.records.len(), 3);
            let mut has_secret_result = false;
            for record in &result.result.records {
                assert!(matches!(record.get("id"), Some(Value::Integer(_))));
                match record.get("value").expect("window result") {
                    Value::Secret(_) => has_secret_result = true,
                    Value::Null => {}
                    exposed => panic!("{table}: {expression} exposed {exposed:?}"),
                }
            }
            assert!(has_secret_result, "{table}: {expression}");
        }
        assert_eq!(
            runtime
                .execute_query(&format!(
                    "WITH w AS (SELECT id, LAG(token) OVER (ORDER BY id) AS prev FROM {table}) SELECT id FROM w WHERE prev = 'synthetic-window-value'"
                ))
                .expect("typed masked window result remains usable")
                .result
                .records
                .len(),
            1
        );
        let sql = format!(
            "WITH w AS (SELECT id, LAG((SELECT token FROM {table} WHERE id = 1), 0) OVER (ORDER BY id) AS value FROM {table}) SELECT id, value FROM w WHERE value = 'synthetic-window-value'"
        );
        let result = runtime
            .execute_query(&sql)
            .expect("sensitive literal window input");
        assert_eq!(result.result.records.len(), 3);
        assert!(result
            .result
            .records
            .iter()
            .all(|record| { record.get("value").expect("value").display_string() == "***" }));
    }
}

#[test]
fn sealed_large_tables_filter_secret_columns_in_the_statement_context() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, _) = open(&directory.path().join("large.rdb"));
    runtime
        .execute_query("SET SECRET token = 'synthetic-large-filter'")
        .expect("reference");
    runtime
        .execute_query("CREATE TABLE typed (id INTEGER, token SECRET)")
        .expect("typed table");
    runtime
        .db()
        .store()
        .create_collection("untyped")
        .expect("untyped collection");
    for table in ["typed", "untyped"] {
        let collection = runtime.db().store().get_collection(table).expect("table");
        for batch in 0..20 {
            let rows = (batch * 512..(batch + 1) * 512)
                .map(|id| format!("({id}, SECRET('synthetic-large-filter'))"))
                .collect::<Vec<_>>()
                .join(", ");
            runtime
                .execute_query(&format!("INSERT INTO {table} (id, token) VALUES {rows}"))
                .expect("insert batch");
            collection.force_seal().expect("seal segment");
        }
        assert_eq!(collection.count(), 10240);
        for predicate in [
            "token = 'synthetic-large-filter'",
            "token LIKE 'synthetic-large%'",
            "token = $secrets.default.token",
        ] {
            let result = runtime
                .execute_query(&format!("SELECT id, token FROM {table} WHERE {predicate}"))
                .expect("large filter");
            assert_eq!(result.result.records.len(), 10240, "{table}: {predicate}");
            assert!(result
                .result
                .records
                .iter()
                .all(|record| { record.get("token").expect("token").display_string() == "***" }));
        }
        assert_eq!(
            value(
                &runtime,
                &format!("SELECT id FROM {table} WHERE token = 'synthetic-large-filter' ORDER BY id DESC LIMIT 1"),
                "id"
            ),
            Value::Integer(10239)
        );
    }
}

#[test]
fn references_and_vault_commands_share_default_and_named_entries() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, _) = open(&directory.path().join("vault.rdb"));
    runtime
        .execute_query("SET SECRET acme.token = 'match-me'")
        .expect("set");
    assert_eq!(
        value(&runtime, "VAULT REVEAL red.vault.acme.token", "value"),
        Value::text("match-me")
    );
    runtime
        .execute_query("VAULT PUT red.vault.acme.token = 'new-value'")
        .expect("put");
    assert_eq!(
        value(
            &runtime,
            "SELECT $secrets.default.acme.token AS value",
            "value"
        )
        .display_string(),
        "***"
    );
    assert_eq!(
        value(&runtime, "SELECT $secret.acme.token AS value", "value").display_string(),
        "***"
    );
    runtime
        .execute_query("CREATE VAULT app WITH OWN MASTER KEY")
        .expect("create");
    runtime
        .execute_query("VAULT PUT app.token = 'named-value'")
        .expect("put named");
    assert_eq!(
        value(&runtime, "SELECT $secrets.app.token AS value", "value").display_string(),
        "***"
    );
    let names = runtime.execute_query("SHOW SECRETS").expect("list");
    assert_eq!(names.result.records.len(), 2);
    runtime
        .execute_query("DELETE SECRET acme.token")
        .expect("delete");
    assert_eq!(
        value(
            &runtime,
            "SELECT $secrets.default.acme.token AS value",
            "value"
        ),
        Value::Null
    );
    assert!(runtime
        .execute_query("VAULT REVEAL red.vault.acme.token")
        .is_err());
}

#[test]
fn secret_columns_filter_on_plaintext_and_reference_updates_stay_encrypted_after_restart() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("columns.rdb");
    let probe = "synthetic_reference_assignment_contract_20261003";
    {
        let (runtime, _) = open(&path);
        runtime
            .execute_query("CREATE TABLE accounts (id INTEGER, token SECRET)")
            .expect("table");
        runtime
            .execute_query(
                "INSERT INTO accounts (id, token) VALUES (1, SECRET('original')), (2, 'plain-input')",
            )
            .expect("insert");
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM accounts WHERE token = 'original'")
                .expect("filter")
                .result
                .records
                .len(),
            1
        );
        runtime
            .execute_query(&format!("SET SECRET replacement = '{probe}'"))
            .expect("source");
        assert_eq!(
            runtime
                .execute_query(
                    "UPDATE accounts SET token = $secrets.default.replacement WHERE id = 1"
                )
                .expect("update")
                .affected_rows,
            1
        );
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM accounts WHERE token = $secrets.default.replacement")
                .expect("reference filter")
                .result
                .records
                .len(),
            1
        );
        assert_eq!(
            value(
                &runtime,
                "SELECT token AS value FROM accounts WHERE id = 1",
                "value"
            )
            .display_string(),
            "***"
        );
        assert!(runtime
            .execute_query("SELECT CAST(token AS INTEGER) FROM accounts")
            .map_or_else(
                |error| !error.to_string().contains(probe),
                |result| !format!("{:?}", result.result).contains(probe)
            ));
        let collection = runtime
            .db()
            .store()
            .get_collection("accounts")
            .expect("accounts");
        for entity in collection.query_all(|_| true) {
            if let EntityData::Row(row) = entity.data {
                assert!(
                    matches!(row.get_field("token"), Some(Value::Secret(_))),
                    "plaintext secret in storage"
                );
            }
        }
        runtime.checkpoint().expect("checkpoint");
        assert!(!contains_plaintext(directory.path(), probe.as_bytes()));
    }
    let (runtime, _) = open(&path);
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM accounts WHERE token = $secrets.default.replacement")
            .expect("restart filter")
            .result
            .records
            .len(),
        1
    );
    assert_eq!(
        value(&runtime, "VAULT REVEAL red.vault.replacement", "value"),
        Value::text(probe)
    );
}

#[test]
fn transformed_and_nested_secret_expressions_are_computed_but_masked() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, _) = open(&directory.path().join("expressions.rdb"));
    runtime
        .execute_query("SET SECRET token = 'eight123'")
        .expect("secret");
    runtime
        .execute_query("CREATE TABLE probes (id INTEGER, token SECRET)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO probes (id, token) VALUES (1, SECRET('eight123'))")
        .expect("row");
    for sql in [
        "SELECT LENGTH($secrets.default.token) AS value FROM probes",
        "SELECT CONCAT('prefix-', $secrets.default.token) AS value FROM probes",
        "SELECT UPPER(token) AS value FROM probes",
        "SELECT MIN(token) AS value FROM probes",
        "SELECT ARRAY_AGG(token) AS value FROM probes",
        "SELECT token AS value FROM probes GROUP BY token",
        "SELECT LAG(token, 0) OVER (ORDER BY id) AS value FROM probes",
        "SELECT LAG(token, 0) OVER (ORDER BY id) AS token FROM probes",
        "SELECT ROW_NUMBER() OVER (ORDER BY token) AS value FROM probes",
    ] {
        assert_eq!(
            value(
                &runtime,
                sql,
                if sql.ends_with("AS token FROM probes") {
                    "token"
                } else {
                    "value"
                }
            )
            .display_string(),
            "***",
            "{sql}"
        );
    }
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM probes WHERE LENGTH($secrets.default.token) = 8")
            .expect("actual length")
            .result
            .records
            .len(),
        1
    );
    assert_eq!(
        runtime
            .execute_query(
                "SELECT COUNT(*) AS value FROM probes WHERE token = $secrets.default.token"
            )
            .expect("aggregate predicate")
            .result
            .records
            .len(),
        1
    );
    assert_eq!(runtime.execute_query("WITH s AS (SELECT LENGTH($secrets.default.token) AS n FROM probes) SELECT n AS value FROM s WHERE n = 8").expect("CTE preserves type and sensitivity").result.records.len(), 1);
    assert_eq!(value(&runtime, "WITH s AS (SELECT LENGTH($secrets.default.token) AS n FROM probes) SELECT n AS value FROM s WHERE n = 8", "value").display_string(), "***");
    assert!(runtime
        .execute_query("UPDATE probes SET id = LENGTH($secrets.default.token)")
        .is_err());

    runtime
        .execute_query("CREATE TABLE matches (token TEXT)")
        .expect("join table");
    runtime
        .execute_query("INSERT INTO matches (token) VALUES ('eight123')")
        .expect("join row");
    assert_eq!(
        runtime
            .execute_query("SELECT p.id FROM probes p JOIN matches m ON p.token = m.token")
            .expect("join uses plaintext")
            .result
            .records
            .len(),
        1
    );
    for predicate in [
        "token LIKE 'eight%'",
        "token IN ('eight123')",
        "token BETWEEN 'eight000' AND 'eight999'",
    ] {
        assert_eq!(
            runtime
                .execute_query(&format!("SELECT id FROM probes WHERE {predicate}"))
                .expect("secret predicate")
                .result
                .records
                .len(),
            1,
            "{predicate}"
        );
    }
}

#[test]
fn tenant_secret_values_versions_lists_and_watch_events_are_isolated() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, _) = open(&directory.path().join("tenants.rdb"));
    runtime.execute_query("CREATE VAULT app").expect("vault");
    runtime
        .execute_query("VAULT PUT app.token = 'platform'")
        .expect("platform secret");
    for (tenant, token) in [("acme", "acme-only"), ("globex", "globex-only")] {
        let _scope = scope(Some(tenant), None, Role::Admin);
        runtime
            .execute_query(&format!("VAULT PUT app.token = '{token}'"))
            .expect("tenant secret");
        runtime
            .execute_query(&format!("SET SECRET token = '{token}'"))
            .expect("default tenant secret");
        assert_eq!(
            value(&runtime, "VAULT REVEAL app.token", "value"),
            Value::text(token)
        );
        assert_eq!(
            value(&runtime, "VAULT GET app.token", "version"),
            Value::Integer(1)
        );
        assert_eq!(
            runtime
                .execute_query("VAULT HISTORY app.token")
                .expect("history")
                .result
                .records
                .len(),
            1
        );
        let events = runtime.vault_watch_events_since("app", "token", 0, 100);
        assert_eq!(events.len(), 1);
        let listed = runtime.execute_query("SHOW SECRETS").expect("list");
        assert_eq!(listed.result.records.len(), 2);
    }
    let _scope = scope(None, None, Role::Admin);
    assert_eq!(
        value(&runtime, "VAULT REVEAL app.token", "value"),
        Value::text("platform")
    );
    assert_eq!(
        value(&runtime, "SELECT $secrets.default.token AS value", "value"),
        Value::Null
    );
}

#[test]
fn use_and_reveal_are_separate_prefix_grants_can_be_revoked_and_deny_applies_to_admin() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, auth) = open(&directory.path().join("permissions.rdb"));
    runtime.execute_query("CREATE VAULT acme").expect("vault");
    runtime
        .execute_query("VAULT PUT acme.a.b.c.token = 'allowed'")
        .expect("descendant");
    runtime
        .execute_query("VAULT PUT acme.a.b.cx.token = 'outside'")
        .expect("outside");
    runtime
        .execute_query("CREATE TABLE probes (id INTEGER, token TEXT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO probes (id, token) VALUES (1, 'allowed'), (2, 'outside')")
        .expect("rows");
    auth.create_user("reader", "synthetic-password", Role::Read)
        .expect("reader");
    {
        let _scope = scope(None, Some("reader"), Role::Read);
        assert!(runtime
            .execute_query("VAULT REVEAL acme.a.b.c.token")
            .expect_err("read role alone does not reveal")
            .to_string()
            .contains("vault:reveal"));
        assert_eq!(
            value(
                &runtime,
                "SELECT $secrets.acme.a.b.c.token AS value",
                "value"
            ),
            Value::Null
        );
    }
    auth.create_user("admin", "synthetic-password", Role::Admin)
        .expect("admin");
    grant(
        &auth,
        UserId::platform("reader"),
        "use-prefix",
        r#"[
        {"effect":"allow","actions":["select"],"resources":["*"]},
        {"effect":"allow","actions":["vault:use"],"resources":["vault:acme.a.b.c.*"]}
    ]"#,
    );
    {
        let _scope = scope(None, Some("admin"), Role::Admin);
        runtime
            .execute_query("VAULT PUT acme.a.b.c.future = 'allowed'")
            .expect("key added after prefix grant");
    }
    {
        let _scope = scope(None, Some("reader"), Role::Read);
        assert!(runtime
            .execute_query("SHOW SECRETS")
            .expect("metadata grants filter listing")
            .result
            .records
            .is_empty());
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM probes WHERE token = $secrets.acme.a.b.c.token")
                .expect("use")
                .result
                .records
                .len(),
            1
        );
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM probes WHERE token = $secrets.acme.a.b.c.future")
                .expect("prefix grant covers future keys")
                .result
                .records
                .len(),
            1
        );
        assert!(runtime
            .execute_query("VAULT REVEAL acme.a.b.c.token")
            .expect_err("use does not reveal")
            .to_string()
            .contains("vault:reveal"));
        assert_eq!(
            value(
                &runtime,
                "SELECT $secrets.acme.a.b.cx.token AS value FROM probes LIMIT 1",
                "value"
            ),
            Value::Null
        );
    }
    auth.detach_policy(PrincipalRef::User(UserId::platform("reader")), "use-prefix")
        .expect("revoke");
    grant(
        &auth,
        UserId::platform("reader"),
        "select-only",
        r#"[{"effect":"allow","actions":["select"],"resources":["*"]}]"#,
    );
    {
        let _scope = scope(None, Some("reader"), Role::Read);
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM probes WHERE token = $secrets.acme.a.b.c.token")
                .expect("revoked")
                .result
                .records
                .len(),
            0
        );
    }
    grant(
        &auth,
        UserId::platform("admin"),
        "deny-reveal",
        r#"[
        {"effect":"allow","actions":["*"],"resources":["*"]},
        {"effect":"deny","actions":["vault:reveal"],"resources":["vault:acme.a.b.c.*"]}
    ]"#,
    );
    let _scope = scope(None, Some("admin"), Role::Admin);
    assert!(runtime
        .execute_query("VAULT REVEAL acme.a.b.c.token")
        .is_err());
}

#[test]
fn tenant_user_grants_use_only_local_entries_and_platform_sharing_is_explicit() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, auth) = open(&directory.path().join("sharing.rdb"));
    runtime.execute_query("CREATE VAULT app").expect("vault");
    runtime
        .execute_query("VAULT PUT app.token = 'shared'")
        .expect("platform");
    runtime
        .execute_query("CREATE TABLE probes (id INTEGER, token TEXT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO probes (id, token) VALUES (1, 'shared'), (2, 'local')")
        .expect("rows");
    {
        let _scope = scope(Some("acme"), None, Role::Admin);
        runtime
            .execute_query("VAULT PUT app.token = 'local'")
            .expect("local");
    }
    auth.create_user_in_tenant(Some("acme"), "a", "synthetic-password", Role::Read)
        .expect("tenant user");
    grant(
        &auth,
        UserId::from_parts(Some("acme"), "a"),
        "tenant-use",
        r#"[
        {"effect":"allow","actions":["select"],"resources":["*"]},
        {"effect":"allow","actions":["vault:use"],"resources":["vault:app.*"]}
    ]"#,
    );
    {
        let _scope = scope(Some("acme"), Some("a"), Role::Read);
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM probes WHERE token = $secrets.app.token")
                .expect("local")
                .result
                .records
                .len(),
            1
        );
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM probes WHERE token = $secrets.platform.app.token")
                .expect("no implicit sharing")
                .result
                .records
                .len(),
            0
        );
        assert!(runtime
            .execute_query("ATTACH POLICY 'tenant-use' TO USER globex.a")
            .is_err());
    }
    grant(
        &auth,
        UserId::from_parts(Some("acme"), "a"),
        "shared-use",
        r#"[
        {"effect":"allow","actions":["vault:use"],"resources":["vault:tenant/platform/app.token"]}
    ]"#,
    );
    let _scope = scope(Some("acme"), Some("a"), Role::Read);
    runtime
        .execute_query("SHOW POLICIES FOR USER a")
        .expect("bare self user resolves inside tenant");
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM probes WHERE token = $secrets.platform.app.token")
            .expect("explicit sharing")
            .result
            .records
            .len(),
        1
    );
    assert!(runtime.execute_query("VAULT REVEAL app.token").is_err());
}

#[test]
fn vault_mutations_in_transactions_are_rejected_and_failed_creation_leaves_no_collection() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("transactions.rdb");
    let runtime = RedDBRuntime::with_options(options(&path)).expect("runtime");
    assert!(runtime.execute_query("CREATE VAULT failed").is_err());
    assert!(runtime.db().store().get_collection("failed").is_none());
    let pager = runtime.db().store().pager().cloned().expect("pager");
    let auth = Arc::new(
        AuthStore::with_vault_certificate(AuthConfig::default(), pager, CERTIFICATE)
            .expect("vault"),
    );
    runtime.set_auth_store(auth);
    runtime.execute_query("CREATE VAULT failed").expect("retry");
    set_current_connection_id(777);
    runtime.execute_query("BEGIN").expect("begin");
    for sql in [
        "SET SECRET token = 'tx-value'",
        "VAULT PUT failed.token = 'tx-value'",
        "CREATE VAULT tx",
    ] {
        assert!(runtime
            .execute_query(sql)
            .expect_err("transaction mutation rejected")
            .to_string()
            .contains("transaction"));
    }
    runtime.execute_query("ROLLBACK").expect("rollback");
    assert_eq!(
        value(&runtime, "SELECT $secrets.default.token AS value", "value"),
        Value::Null
    );
    assert!(runtime.db().store().get_collection("tx").is_none());
}

#[test]
fn bootstrap_preserves_chosen_certificate_and_passphrase_survives_reopen_and_export() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let runtime = RedDBRuntime::with_options(options(&directory.path().join("certificate.rdb")))
        .expect("runtime");
    let pager = runtime.db().store().pager().cloned().expect("pager");
    let auth =
        AuthStore::with_vault_certificate(AuthConfig::default(), Arc::clone(&pager), CERTIFICATE)
            .expect("certificate");
    assert_eq!(
        auth.bootstrap("admin", "synthetic-login-password")
            .expect("bootstrap")
            .certificate
            .as_deref(),
        Some(CERTIFICATE)
    );
    assert!(AuthStore::with_vault_certificate(
        AuthConfig::default(),
        Arc::clone(&pager),
        CERTIFICATE
    )
    .expect("reopen")
    .is_bootstrapped());
    assert!(
        AuthStore::with_vault_certificate(AuthConfig::default(), pager, "human-password")
            .err()
            .expect("malformed certificate")
            .to_string()
            .contains("hexadecimal")
    );

    let runtime = RedDBRuntime::with_options(options(&directory.path().join("passphrase.rdb")))
        .expect("runtime");
    let pager = runtime.db().store().pager().cloned().expect("pager");
    let auth = AuthStore::with_vault_passphrase(
        AuthConfig::default(),
        Arc::clone(&pager),
        "minha senha arbitraria",
    )
    .expect("passphrase");
    assert!(auth
        .bootstrap("admin", "independent-login-password")
        .expect("bootstrap")
        .certificate
        .is_none());
    assert!(AuthStore::with_vault_passphrase(
        AuthConfig::default(),
        Arc::clone(&pager),
        "wrong-password"
    )
    .is_err());
    assert!(AuthStore::with_vault_certificate(
        AuthConfig::default(),
        Arc::clone(&pager),
        CERTIFICATE
    )
    .is_err());
    assert!(AuthStore::with_vault_passphrase(
        AuthConfig::default(),
        Arc::clone(&pager),
        "minha senha arbitraria"
    )
    .expect("passphrase reopen")
    .is_bootstrapped());
    let vault = Vault::with_passphrase(&pager, "minha senha arbitraria").expect("vault");
    let state = VaultState {
        kv: std::collections::HashMap::from([("probe".into(), "synthetic-secret".into())]),
        ..VaultState::default()
    };
    let exported = vault.seal_logical_export(&state).expect("export");
    assert_eq!(
        Vault::unseal_logical_export_with_passphrase(&exported, "minha senha arbitraria")
            .expect("import")
            .kv["probe"],
        "synthetic-secret"
    );
    assert!(Vault::unseal_logical_export_with_passphrase(&exported, "wrong-password").is_err());
}

#[test]
fn tenant_admin_policies_and_bare_users_are_scoped_to_the_session() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, auth) = open(&directory.path().join("policy-tenant.rdb"));
    runtime.execute_query("CREATE VAULT app").expect("vault");
    runtime
        .execute_query("VAULT PUT app.token = 'platform-only'")
        .expect("platform");
    auth.create_user("a", "synthetic-password", Role::Read)
        .expect("platform a");
    auth.create_user_in_tenant(Some("acme"), "a", "synthetic-password", Role::Read)
        .expect("tenant a");
    auth.create_user_in_tenant(Some("acme"), "owner", "synthetic-password", Role::Admin)
        .expect("tenant owner");
    {
        let _scope = scope(Some("acme"), None, Role::Admin);
        runtime
            .execute_query("VAULT PUT app.token = 'local-only'")
            .expect("local");
    }
    let mut policy = Policy::from_json_str(r#"{"id":"local-owner","version":1,"statements":[{"effect":"allow","actions":["*"],"resources":["*"]}]}"#).expect("policy");
    policy.tenant = Some("acme".into());
    auth.put_policy(policy).expect("owner policy");
    auth.attach_policy(
        PrincipalRef::User(UserId::from_parts(Some("acme"), "owner")),
        "local-owner",
    )
    .expect("attach");
    {
        let _scope = scope(Some("acme"), Some("owner"), Role::Admin);
        assert_eq!(
            value(
                &runtime,
                "SELECT $secrets.platform.app.token AS value",
                "value"
            ),
            Value::Null
        );
        runtime.execute_query(r#"CREATE POLICY 'local-use' AS '{"id":"local-use","version":1,"statements":[{"effect":"allow","actions":["select","vault:use"],"resources":["*"]}]}'"#).expect("create local policy");
        runtime
            .execute_query("ATTACH POLICY 'local-use' TO USER a")
            .expect("bare user in current tenant");
        assert!(runtime
            .execute_query("ATTACH POLICY 'local-use' TO USER globex.a")
            .expect_err("cross tenant")
            .to_string()
            .contains("across tenants"));
    }
    assert!(auth.effective_policies(&UserId::platform("a")).is_empty());
    assert_eq!(
        auth.get_policy("local-use")
            .expect("policy")
            .tenant
            .as_deref(),
        Some("acme")
    );
    let _scope = scope(Some("acme"), Some("a"), Role::Read);
    assert_eq!(
        value(&runtime, "SELECT $secrets.app.token AS value", "value").display_string(),
        "***"
    );
    assert_eq!(
        value(
            &runtime,
            "SELECT $secrets.platform.app.token AS value",
            "value"
        ),
        Value::Null
    );
}

#[test]
fn untyped_secret_columns_preserve_encryption_and_aggregation_sensitivity() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, _) = open(&directory.path().join("untyped.rdb"));
    runtime
        .db()
        .store()
        .create_collection("loose")
        .expect("untyped legacy collection");
    runtime
        .execute_query("INSERT INTO loose (id, token) VALUES (1, SECRET('original'))")
        .expect("insert");
    runtime
        .execute_query("UPDATE loose SET token = 'replacement'")
        .expect("update retains encryption");
    assert_eq!(
        value(&runtime, "SELECT token AS value FROM loose", "value").display_string(),
        "***"
    );
    assert_eq!(
        value(&runtime, "SELECT MIN(token) AS value FROM loose", "value").display_string(),
        "***"
    );
    assert_eq!(
        value(
            &runtime,
            "SELECT token AS value FROM loose GROUP BY token",
            "value"
        )
        .display_string(),
        "***"
    );
    assert_eq!(
        value(&runtime, "SELECT MIN(token) FROM loose", "min(token)").display_string(),
        "***"
    );
    let ordered = runtime
        .execute_query("SELECT COUNT(*) AS count FROM loose GROUP BY id ORDER BY MIN(token)")
        .expect("hidden order aggregate");
    assert!(
        !format!("{:?}", ordered.result.records).contains("replacement"),
        "{:?}",
        ordered.result.records
    );
    let aggregate = runtime
        .execute_query(
            "SELECT COUNT(*) AS count FROM loose GROUP BY id HAVING MIN(token) = 'replacement'",
        )
        .expect("hidden secret aggregate");
    assert!(
        !format!("{:?}", aggregate.result.records).contains("replacement"),
        "{:?}",
        aggregate.result.records
    );
    assert_eq!(aggregate.result.records.len(), 1);
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM loose WHERE token = 'replacement'")
            .expect("filter on real value")
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn concurrent_vault_writes_allocate_unique_ordered_versions() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, _) = open(&directory.path().join("concurrent.rdb"));
    runtime.execute_query("CREATE VAULT app").expect("vault");
    let runtime = Arc::new(runtime);
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|writer| {
            let runtime = Arc::clone(&runtime);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for version in 0..4 {
                    runtime
                        .execute_query(&format!(
                            "VAULT PUT app.token = 'writer-{writer}-{version}'"
                        ))
                        .expect("write");
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("writer");
    }
    let history = runtime
        .execute_query("VAULT HISTORY app.token")
        .expect("history");
    let versions: Vec<_> = history
        .result
        .records
        .iter()
        .map(|row| row.get("version").cloned().expect("version"))
        .collect();
    assert_eq!(versions, (1..=32).map(Value::Integer).collect::<Vec<_>>());
}

#[test]
fn legacy_platform_secrets_remain_usable_without_exposing_internal_keys_or_resurrecting_purged_values(
) {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, auth) = open(&directory.path().join("legacy.rdb"));
    auth.vault_kv_try_set("old.token".into(), "legacy-value".into())
        .expect("legacy fixture");
    auth.vault_kv_try_set("red.iam.synthetic".into(), "internal-value".into())
        .expect("internal fixture");
    assert_eq!(
        value(&runtime, "VAULT REVEAL red.vault.old.token", "value"),
        Value::text("legacy-value")
    );
    assert_eq!(
        value(&runtime, "SELECT $secret.old.token AS value", "value").display_string(),
        "***"
    );
    assert_eq!(
        value(
            &runtime,
            "SELECT $secrets.default.red.iam.synthetic AS value",
            "value"
        ),
        Value::Null
    );
    runtime
        .execute_query("SET SECRET old.token = 'new-value'")
        .expect("rewrite");
    runtime
        .execute_query("VAULT PURGE red.vault.old.token")
        .expect("purge");
    assert_eq!(
        value(&runtime, "SELECT $secret.old.token AS value", "value"),
        Value::Null
    );
    assert!(auth.vault_kv_get("old.token").is_none());
    runtime
        .execute_query("CREATE TABLE public_probe (id INTEGER)")
        .expect("table");
    for sql in [
        "SELECT key FROM red.vault",
        "SELECT p.id FROM public_probe p JOIN red.vault v ON p.id = v.version",
        "SELECT id FROM public_probe WHERE id IN (SELECT version FROM red.vault)",
        "UPDATE red.vault SET value = 'plaintext'",
        "DELETE FROM red.vault",
    ] {
        assert!(runtime.execute_query(sql).is_err(), "{sql}");
    }
}

#[test]
fn native_vault_dump_restores_versions_tenants_and_metadata_with_imported_keys() {
    let _scope = scope(None, None, Role::Admin);
    let directory = tempfile::tempdir().expect("directory");
    let (source, source_auth) = open(&directory.path().join("source.rdb"));
    source
        .execute_query("CREATE VAULT app WITH OWN MASTER KEY")
        .expect("vault");
    source
        .execute_query("VAULT PUT app.token = 'platform-v1' TAGS ['scope-tag']")
        .expect("initial version");
    source
        .execute_query("VAULT ROTATE app.token = 'platform-v2'")
        .expect("rotate");
    {
        let _scope = scope(Some("acme"), None, Role::Admin);
        source
            .execute_query("VAULT PUT app.token = 'tenant-value'")
            .expect("tenant");
    }
    let records = source
        .export_vault_collection_records("app")
        .expect("native dump");
    assert_eq!(records.len(), 3);
    assert!(!records.join("").contains("platform-v1"));
    let (destination, destination_auth) = open(&directory.path().join("destination.rdb"));
    destination_auth
        .vault_kv_try_import(source_auth.vault_kv_snapshot())
        .expect("import source keys");
    assert!(destination
        .import_vault_collection_records(
            "invalid",
            &["00".into()],
            source.db().store().format_version()
        )
        .is_err());
    assert!(destination.db().store().get_collection("invalid").is_none());
    assert_eq!(
        destination
            .import_vault_collection_records("app", &records, source.db().store().format_version())
            .expect("restore"),
        3
    );
    let restored_history = destination
        .execute_query("VAULT HISTORY app.token")
        .expect("restored metadata");
    assert!(format!("{:?}", restored_history.result.records[0].get("tags")).contains("scope-tag"));
    assert_eq!(
        value(&destination, "VAULT REVEAL app.token", "value"),
        Value::text("platform-v2")
    );
    assert_eq!(
        value(&destination, "VAULT REVEAL app.token VERSION 1", "value"),
        Value::text("platform-v1")
    );
    assert_eq!(
        destination
            .execute_query("VAULT HISTORY app.token")
            .expect("history")
            .result
            .records
            .len(),
        2
    );
    {
        let _scope = scope(Some("acme"), None, Role::Admin);
        assert_eq!(
            value(&destination, "VAULT REVEAL app.token", "value"),
            Value::text("tenant-value")
        );
    }
    assert!(destination
        .import_vault_collection_records("app", &records, source.db().store().format_version())
        .is_err());
    destination.checkpoint().expect("checkpoint");
}
