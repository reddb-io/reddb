//! Local client-to-server Vault flows; no external services or opt-in environment flags.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use reddb::auth::policies::Policy;
use reddb::auth::store::PrincipalRef;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::health::HealthProvider;
use reddb::storage::{DeployProfile, StoragePackaging, StorageProfileSelection};
use reddb::wire::redwire::start_redwire_listener_on;
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_client::redwire::{Auth, ConnectOptions, RedWireClient};
use reddb_client::{QueryResult, ValueOut};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const VAULT_PASSWORD: &str = "minha senha arbitrária ' com espaços";
const LOGIN_PASSWORD: &str = "synthetic-login-password";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

struct Server {
    address: SocketAddr,
    runtime: Arc<RedDBRuntime>,
    auth: Option<Arc<AuthStore>>,
    listener: JoinHandle<()>,
}

fn runtime(path: &Path) -> RedDBRuntime {
    let options = RedDBOptions::persistent(path)
        .with_storage_profile(StorageProfileSelection {
            deploy_profile: DeployProfile::Embedded,
            packaging: StoragePackaging::OperationalDirectory,
            replica_count: 0,
            managed_backup: false,
            wal_retention: false,
        })
        .expect("operational storage profile");
    RedDBRuntime::with_options(options).expect("persistent runtime")
}

impl Server {
    async fn start(path: &Path, unlock: bool) -> Self {
        let runtime = Arc::new(runtime(path));
        let auth = unlock.then(|| {
            let auth = Arc::new(
                AuthStore::with_vault_passphrase(
                    AuthConfig {
                        enabled: true,
                        vault_enabled: true,
                        ..AuthConfig::default()
                    },
                    runtime.db().store().pager().expect("paged store").clone(),
                    VAULT_PASSWORD,
                )
                .expect("unlock with arbitrary password"),
            );
            if !auth.is_bootstrapped() {
                auth.bootstrap("admin", LOGIN_PASSWORD).expect("bootstrap");
                // Match the production preset's explicit first-admin grant.
                let policy = Policy::from_json_str(r#"{"id":"first-admin","version":1,"statements":[{"effect":"allow","actions":["*"],"resources":["*"]}]}"#).expect("bootstrap policy");
                auth.put_policy(policy).expect("bootstrap policy persisted");
                auth.attach_policy(PrincipalRef::User(UserId::platform("admin")), "first-admin")
                    .expect("first admin grant");
            }
            auth.ensure_vault_secret_key();
            runtime.set_auth_store(Arc::clone(&auth));
            auth
        });
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let address = listener.local_addr().expect("ephemeral address");
        let runtime_for_listener = Arc::clone(&runtime);
        let listener = tokio::spawn(async move {
            start_redwire_listener_on(listener, runtime_for_listener)
                .await
                .expect("RedWire listener");
        });
        Self {
            address,
            runtime,
            auth,
            listener,
        }
    }

    async fn connect(&self, authentication: Auth) -> RedWireClient {
        timeout(
            REQUEST_TIMEOUT,
            RedWireClient::connect(
                ConnectOptions::new(self.address.ip().to_string(), self.address.port())
                    .with_auth(authentication),
            ),
        )
        .await
        .expect("handshake deadline")
        .expect("authenticated client")
    }

    async fn admin(&self) -> RedWireClient {
        self.connect(Auth::Basic {
            user: "admin".into(),
            pass: LOGIN_PASSWORD.into(),
        })
        .await
    }

    async fn tenant_client(&self, tenant: &str, username: &str) -> RedWireClient {
        let session = self
            .auth
            .as_ref()
            .expect("unlocked auth")
            .authenticate_in_tenant(Some(tenant), username, LOGIN_PASSWORD)
            .expect("tenant login");
        self.connect(Auth::Bearer(session.token)).await
    }

    async fn wait_for_connections(&self, count: usize) {
        timeout(REQUEST_TIMEOUT, async {
            loop {
                if self
                    .runtime
                    .health()
                    .diagnostics
                    .get("runtime.active_connections")
                    == Some(&count.to_string())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("disconnected sessions release their runtime leases");
    }

    async fn stop(mut self) {
        self.wait_for_connections(0).await;
        self.listener.abort();
        let _ = (&mut self.listener).await;
        self.runtime
            .checkpoint()
            .expect("checkpoint before restart");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

async fn query(client: &mut RedWireClient, sql: &str) -> QueryResult {
    timeout(REQUEST_TIMEOUT, client.query(sql))
        .await
        .expect("query deadline")
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

async fn value(client: &mut RedWireClient, sql: &str) -> ValueOut {
    let result = query(client, sql).await;
    result
        .rows
        .first()
        .and_then(|row| row.iter().find(|(name, _)| name == "value"))
        .expect("value column")
        .1
        .clone()
}

async fn denied(client: &mut RedWireClient, sql: &str, reason: &str) {
    let error = timeout(REQUEST_TIMEOUT, client.query(sql))
        .await
        .expect("denied query deadline")
        .expect_err("operation must be denied");
    assert!(error.to_string().contains(reason), "{sql}: {error}");
}

async fn policy(client: &mut RedWireClient, id: &str, actions: &str, resource: &str) {
    query(client, &format!(r#"CREATE POLICY '{id}' AS '{{"id":"{id}","version":1,"statements":[{{"effect":"allow","actions":{actions},"resources":["{resource}"]}}]}}'"#)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_queries_use_prefix_secrets_without_revealing_them_and_grants_are_revocable() {
    let directory = tempfile::tempdir().expect("directory");
    let server = Server::start(&directory.path().join("permissions.rdb"), true).await;
    let mut admin = server.admin().await;
    query(
        &mut admin,
        "CREATE USER reader PASSWORD 'synthetic-login-password' ROLE read",
    )
    .await;
    query(&mut admin, "CREATE VAULT app").await;
    query(
        &mut admin,
        "VAULT PUT app.a.b.c.token = 'synthetic-allowed'",
    )
    .await;
    query(
        &mut admin,
        "VAULT PUT app.a.b.cx.token = 'synthetic-outside'",
    )
    .await;
    query(&mut admin, "CREATE TABLE probes (id INT, token TEXT)").await;
    query(
        &mut admin,
        "INSERT INTO probes (id, token) VALUES (1, 'synthetic-allowed'), (2, 'synthetic-outside')",
    )
    .await;
    let mut reader = server
        .connect(Auth::Basic {
            user: "reader".into(),
            pass: LOGIN_PASSWORD.into(),
        })
        .await;
    assert_eq!(
        value(&mut reader, "SELECT $secrets.app.a.b.c.token AS value").await,
        ValueOut::Null
    );
    denied(&mut reader, "VAULT REVEAL app.a.b.c.token", "vault:reveal").await;
    policy(&mut admin, "query-access", r#"["select"]"#, "*").await;
    query(&mut admin, "ATTACH POLICY 'query-access' TO USER reader").await;
    policy(
        &mut admin,
        "prefix-use",
        r#"["vault:use"]"#,
        "vault:app.a.b.c.*",
    )
    .await;
    query(&mut admin, "ATTACH POLICY 'prefix-use' TO USER reader").await;
    query(
        &mut admin,
        "VAULT PUT app.a.b.c.future = 'synthetic-allowed'",
    )
    .await;
    for key in ["token", "future"] {
        assert_eq!(
            value(
                &mut reader,
                &format!("SELECT $secrets.app.a.b.c.{key} AS value")
            )
            .await,
            ValueOut::String("***".into())
        );
    }
    let selected = query(&mut reader, "SELECT id, LENGTH($secrets.app.a.b.c.token) AS value FROM probes WHERE token = $secrets.app.a.b.c.token").await;
    assert_eq!(
        selected.rows,
        vec![vec![
            ("id".into(), ValueOut::Integer(1)),
            ("value".into(), ValueOut::String("***".into()))
        ]]
    );
    assert!(!format!("{selected:?}").contains("synthetic-allowed"));
    assert_eq!(
        value(&mut reader, "SELECT $secrets.app.a.b.cx.token AS value").await,
        ValueOut::Null
    );
    denied(&mut reader, "VAULT REVEAL app.a.b.c.token", "vault:reveal").await;
    policy(
        &mut admin,
        "prefix-reveal",
        r#"["vault:reveal"]"#,
        "vault:app.a.b.c.*",
    )
    .await;
    query(&mut admin, "ATTACH POLICY 'prefix-reveal' TO USER reader").await;
    assert_eq!(
        value(&mut reader, "VAULT REVEAL app.a.b.c.token").await,
        ValueOut::String("synthetic-allowed".into())
    );
    query(
        &mut admin,
        "VAULT ROTATE app.a.b.c.token = 'synthetic-rotated'",
    )
    .await;
    denied(
        &mut reader,
        "VAULT REVEAL app.a.b.c.token VERSION 1",
        "vault:reveal_history",
    )
    .await;
    policy(
        &mut admin,
        "prefix-history",
        r#"["vault:reveal_history"]"#,
        "vault:app.a.b.c.*",
    )
    .await;
    query(&mut admin, "ATTACH POLICY 'prefix-history' TO USER reader").await;
    assert_eq!(
        value(&mut reader, "VAULT REVEAL app.a.b.c.token VERSION 1").await,
        ValueOut::String("synthetic-allowed".into())
    );
    query(&mut admin, "DETACH POLICY 'prefix-reveal' FROM USER reader").await;
    denied(&mut reader, "VAULT REVEAL app.a.b.c.token", "vault:reveal").await;
    query(&mut admin, "DETACH POLICY 'prefix-use' FROM USER reader").await;
    assert_eq!(
        value(&mut reader, "SELECT $secrets.app.a.b.c.token AS value").await,
        ValueOut::Null
    );
    reader.close().await.expect("reader close");
    admin.close().await.expect("admin close");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tenant_owner_grants_a_bare_local_username_without_granting_other_tenants() {
    let directory = tempfile::tempdir().expect("directory");
    let server = Server::start(&directory.path().join("tenants.rdb"), true).await;
    let auth = server.auth.as_ref().expect("auth");
    auth.create_user("a", LOGIN_PASSWORD, Role::Read)
        .expect("platform reader");
    for tenant in ["acme", "globex"] {
        auth.create_user_in_tenant(Some(tenant), "a", LOGIN_PASSWORD, Role::Read)
            .expect("tenant reader");
        auth.create_user_in_tenant(Some(tenant), "owner", LOGIN_PASSWORD, Role::Admin)
            .expect("tenant owner");
        let id = format!("{tenant}-owner");
        let mut owner_policy = Policy::from_json_str(&format!(r#"{{"id":"{id}","version":1,"statements":[{{"effect":"allow","actions":["*"],"resources":["*"]}}]}}"#)).expect("owner policy");
        owner_policy.tenant = Some(tenant.into());
        auth.put_policy(owner_policy)
            .expect("owner policy persisted");
        auth.attach_policy(
            PrincipalRef::User(UserId::from_parts(Some(tenant), "owner")),
            &id,
        )
        .expect("owner access");
    }
    let mut admin = server.admin().await;
    query(&mut admin, "CREATE VAULT app").await;
    query(&mut admin, "VAULT PUT app.a.b.c.token = 'platform-value'").await;
    let mut owner = server.tenant_client("acme", "owner").await;
    let mut other_owner = server.tenant_client("globex", "owner").await;
    query(&mut owner, "VAULT PUT app.a.b.c.token = 'acme-value'").await;
    query(
        &mut other_owner,
        "VAULT PUT app.a.b.c.token = 'globex-value'",
    )
    .await;
    policy(&mut owner, "local-query", r#"["select"]"#, "*").await;
    policy(
        &mut owner,
        "local-use",
        r#"["vault:use"]"#,
        "vault:app.a.b.c.*",
    )
    .await;
    for id in ["local-query", "local-use"] {
        query(&mut owner, &format!("ATTACH POLICY '{id}' TO USER a")).await;
    }
    denied(
        &mut owner,
        "ATTACH POLICY 'local-use' TO USER globex.a",
        "across tenants",
    )
    .await;
    let mut local = server.tenant_client("acme", "a").await;
    let mut other = server.tenant_client("globex", "a").await;
    let mut platform = server
        .connect(Auth::Basic {
            user: "a".into(),
            pass: LOGIN_PASSWORD.into(),
        })
        .await;
    assert_eq!(
        value(&mut local, "SELECT $secrets.app.a.b.c.token AS value").await,
        ValueOut::String("***".into())
    );
    for client in [&mut other, &mut platform] {
        assert_eq!(
            value(client, "SELECT $secrets.app.a.b.c.token AS value").await,
            ValueOut::Null
        );
    }
    assert_eq!(
        value(
            &mut local,
            "SELECT $secrets.platform.app.a.b.c.token AS value"
        )
        .await,
        ValueOut::Null
    );
    denied(&mut local, "VAULT REVEAL app.a.b.c.token", "vault:reveal").await;
    policy(
        &mut owner,
        "local-reveal",
        r#"["vault:reveal"]"#,
        "vault:app.a.b.c.*",
    )
    .await;
    query(&mut owner, "ATTACH POLICY 'local-reveal' TO USER a").await;
    assert_eq!(
        value(&mut local, "VAULT REVEAL app.a.b.c.token").await,
        ValueOut::String("acme-value".into())
    );
    assert_eq!(
        value(&mut other_owner, "VAULT REVEAL app.a.b.c.token").await,
        ValueOut::String("globex-value".into())
    );
    assert_eq!(
        value(&mut admin, "VAULT REVEAL app.a.b.c.token").await,
        ValueOut::String("platform-value".into())
    );
    for client in [local, other, platform, owner, other_owner, admin] {
        client.close().await.expect("close tenant client");
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_connections_have_independent_vault_transactions_and_disconnect_rolls_back() {
    let directory = tempfile::tempdir().expect("directory");
    let server = Server::start(&directory.path().join("transactions.rdb"), true).await;
    let mut writer = server.admin().await;
    let mut observer = server.admin().await;
    query(&mut writer, "SET SECRET token = 'original'").await;
    query(&mut writer, "BEGIN").await;
    query(&mut writer, "SET SECRET token = 'pending'").await;
    assert_eq!(
        value(&mut writer, "VAULT REVEAL red.vault.token").await,
        ValueOut::String("pending".into())
    );
    assert_eq!(
        value(&mut observer, "VAULT REVEAL red.vault.token").await,
        ValueOut::String("original".into())
    );
    query(&mut observer, "BEGIN").await;
    query(&mut observer, "SET SECRET separate = 'other-transaction'").await;
    query(&mut observer, "ROLLBACK").await;
    query(&mut writer, "SAVEPOINT before_change").await;
    query(&mut writer, "SET SECRET token = 'discarded'").await;
    query(&mut writer, "ROLLBACK TO SAVEPOINT before_change").await;
    assert_eq!(
        value(&mut writer, "VAULT REVEAL red.vault.token").await,
        ValueOut::String("pending".into())
    );
    query(&mut writer, "COMMIT").await;
    assert_eq!(
        value(&mut observer, "VAULT REVEAL red.vault.token").await,
        ValueOut::String("pending".into())
    );
    assert_eq!(
        value(&mut observer, "SELECT $secrets.default.separate AS value").await,
        ValueOut::Null
    );
    query(&mut writer, "BEGIN").await;
    query(&mut writer, "SET SECRET token = 'abandoned'").await;
    drop(writer); // Drop the socket without sending Bye or ROLLBACK.
    server.wait_for_connections(1).await;
    let mut reused = server.admin().await;
    query(&mut reused, "BEGIN").await;
    assert_eq!(
        value(&mut reused, "VAULT REVEAL red.vault.token").await,
        ValueOut::String("pending".into())
    );
    query(&mut reused, "COMMIT").await;
    assert_eq!(
        query(&mut observer, "VAULT HISTORY red.vault.token")
            .await
            .rows
            .len(),
        2
    );
    reused.close().await.expect("close reused connection");
    observer.close().await.expect("close observer");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_bootstrap_password_sealed_restart_and_unlocked_client_round_trip() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("password.rdb");
    let mut bootstrap = Command::new(env!("CARGO_BIN_EXE_red"))
        .args([
            "bootstrap",
            "--vault",
            "--username",
            "admin",
            "--password-stdin",
            "--json",
            "--path",
        ])
        .arg(&path)
        .env("REDDB_VAULT_PASSPHRASE", VAULT_PASSWORD)
        .env_remove("REDDB_VAULT_PASSPHRASE_FILE")
        .env_remove("REDDB_CERTIFICATE")
        .env_remove("REDDB_CERTIFICATE_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("bootstrap CLI");
    bootstrap
        .stdin
        .take()
        .expect("password stdin")
        .write_all(format!("{LOGIN_PASSWORD}\n").as_bytes())
        .expect("write login password");
    let output = bootstrap.wait_with_output().expect("bootstrap result");
    assert!(
        output.status.success(),
        "bootstrap: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 CLI output");
    assert!(!stdout.contains(VAULT_PASSWORD));
    assert!(!stdout.contains(LOGIN_PASSWORD));
    let server = Server::start(&path, true).await;
    let mut admin = server.admin().await;
    query(&mut admin, "CREATE VAULT app").await;
    query(
        &mut admin,
        "VAULT PUT app.token = 'synthetic-persisted-value'",
    )
    .await;
    assert_eq!(
        value(&mut admin, "SELECT $secrets.app.token AS value").await,
        ValueOut::String("***".into())
    );
    admin.close().await.expect("close before sealed restart");
    server.stop().await;

    let sealed = Server::start(&path, false).await;
    let mut anonymous = sealed.connect(Auth::Anonymous).await;
    assert_eq!(
        value(&mut anonymous, "SELECT $secrets.app.token AS value").await,
        ValueOut::Null
    );
    denied(&mut anonymous, "VAULT REVEAL app.token", "sealed").await;
    anonymous.close().await.expect("close sealed client");
    sealed.stop().await;
    {
        let runtime = runtime(&path);
        assert!(AuthStore::with_vault_passphrase(
            AuthConfig::default(),
            runtime.db().store().pager().expect("pager").clone(),
            "wrong-password",
        )
        .is_err());
    }
    let unlocked = Server::start(&path, true).await;
    let mut admin = unlocked.admin().await;
    assert_eq!(
        value(&mut admin, "VAULT REVEAL app.token").await,
        ValueOut::String("synthetic-persisted-value".into())
    );
    assert_eq!(
        query(&mut admin, "VAULT HISTORY app.token")
            .await
            .rows
            .len(),
        1
    );
    admin.close().await.expect("close unlocked client");
    unlocked.stop().await;
}
