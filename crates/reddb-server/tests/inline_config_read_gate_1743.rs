//! #1743 — the inline `$config` / `CONFIG()` / `KV('red_config', …)` resolver
//! must gate reads on `config:read`, mirroring the `$secret` sibling. An
//! authenticated tenant without `config:read` is refused (deny → SQL NULL);
//! a principal that holds `config:read` still reads the value.

use std::sync::Arc;

use reddb_server::auth::policies::Policy;
use reddb_server::auth::store::{AuthStore, PrincipalRef};
use reddb_server::auth::{AuthConfig, Role, UserId};
use reddb_server::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb_server::RedDBRuntime;

fn allow_policy(id: &str, action: &str, resource: &str) -> Policy {
    Policy::from_json_str(&format!(
        r#"{{
            "id":"{id}",
            "version":1,
            "statements":[{{
                "effect":"allow",
                "actions":["{action}"],
                "resources":["{resource}"]
            }}]
        }}"#
    ))
    .unwrap()
}

fn put_policy(auth: &AuthStore, policy: Policy) -> String {
    let id = policy.id.clone();
    auth.put_policy(policy).expect("put policy");
    id
}

fn attach(auth: &AuthStore, user: &UserId, policy_id: &str) {
    auth.attach_policy(PrincipalRef::User(user.clone()), policy_id)
        .expect("attach policy");
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

/// Whether the inline resolver expression matched the staged secret. The
/// predicate `'sk-secret' = <resolver>` yields one row when the resolver
/// returns the value and zero rows when it is denied (→ SQL NULL, which never
/// compares equal).
fn resolver_matches(rt: &RedDBRuntime, resolver_expr: &str) -> bool {
    let sql = format!("SELECT id FROM t WHERE 'sk-secret' = {resolver_expr}");
    let result = rt
        .execute_query(&sql)
        .unwrap_or_else(|err| panic!("query `{sql}` should execute: {err}"));
    !result.result.records.is_empty()
}

#[test]
fn inline_config_resolver_gates_on_config_read() {
    let rt = RedDBRuntime::in_memory().expect("runtime boots");

    // Seed a table row and a sensitive config value before IAM is engaged.
    rt.execute_query("CREATE TABLE t (id INT)")
        .expect("create t");
    rt.execute_query("INSERT INTO t (id) VALUES (1)")
        .expect("insert t");
    rt.execute_query("SET CONFIG ai.openrouter.default.key = 'sk-secret'")
        .expect("stage config key");

    // Wire IAM: `reader` holds `config:read`, `tenant` does not. The first
    // policy write flips `iam_authorization_enabled` on.
    let auth = Arc::new(AuthStore::new(AuthConfig::default()));
    auth.create_user("reader", "p", Role::Read).expect("reader");
    auth.create_user("tenant", "p", Role::Read).expect("tenant");
    // Both principals may read table `t`; only `reader` additionally holds
    // `config:read`, isolating the inline-config gate as the sole difference.
    let table_read = put_policy(&auth, allow_policy("p-table-read", "select", "table:*"));
    let config_read = put_policy(
        &auth,
        allow_policy("p-config-read", "config:read", "config:*"),
    );
    attach(&auth, &UserId::platform("reader"), &table_read);
    attach(&auth, &UserId::platform("reader"), &config_read);
    attach(&auth, &UserId::platform("tenant"), &table_read);
    rt.set_auth_store(auth);

    // Tenant without `config:read`: both inline paths deny → SQL NULL.
    as_user("tenant", Role::Read, || {
        assert!(
            !resolver_matches(&rt, "$config.ai.openrouter.default.key"),
            "$config read must be denied without config:read"
        );
        assert!(
            !resolver_matches(&rt, "KV('red_config', 'ai.openrouter.default.key')"),
            "KV('red_config', …) read must be denied without config:read"
        );
    });

    // Principal with `config:read`: the value resolves through both paths.
    as_user("reader", Role::Read, || {
        assert!(
            resolver_matches(&rt, "$config.ai.openrouter.default.key"),
            "$config read must succeed with config:read"
        );
        assert!(
            resolver_matches(&rt, "KV('red_config', 'ai.openrouter.default.key')"),
            "KV('red_config', …) read must succeed with config:read"
        );
    });
}
