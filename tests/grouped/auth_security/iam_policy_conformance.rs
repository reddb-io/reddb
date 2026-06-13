//! Fixed conformance corpus for IAM column policy enforcement.
//!
//! Each corpus row pins one public query path against one data model and
//! action. The goal is breadth over bespoke assertions: a row is either
//! allowed or must fail with the denied IAM resource named in the error.

use std::collections::BTreeSet;
use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
    set_current_tenant,
};
use reddb::{RedDBError, RedDBOptions, RedDBRuntime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Model {
    Relational,
    Document,
    Vector,
    Graph,
    Timeseries,
}

#[derive(Debug, Clone, Copy)]
enum Fixture {
    Users,
    UsersJoinOrders,
    Accounts,
    Orders,
    TenantEvents,
    Docs,
    Embeddings,
    SocialGraph,
    Metrics,
}

#[derive(Debug, Clone, Copy)]
enum Principal {
    PlatformAlice,
    TenantAlice(&'static str),
}

#[derive(Debug, Clone, Copy)]
enum Expected {
    Allow,
    Deny { resource: &'static str },
}

#[derive(Debug, Clone, Copy)]
struct Case {
    name: &'static str,
    model: Model,
    action: &'static str,
    path: &'static str,
    fixture: Fixture,
    principal: Principal,
    statements: &'static str,
    sql: &'static str,
    expected: Expected,
}

fn runtime_with_auth() -> (RedDBRuntime, Arc<AuthStore>) {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Write).unwrap();
    rt.set_auth_store(Arc::clone(&store));
    (rt, store)
}

fn as_principal<T>(principal: Principal, f: impl FnOnce() -> T) -> T {
    match principal {
        Principal::PlatformAlice => {
            set_current_auth_identity("alice".to_string(), Role::Write);
            let out = f();
            clear_current_auth_identity();
            out
        }
        Principal::TenantAlice(tenant) => {
            set_current_tenant(tenant.to_string());
            set_current_auth_identity("alice".to_string(), Role::Write);
            let out = f();
            clear_current_auth_identity();
            clear_current_tenant();
            out
        }
    }
}

fn attach_case_policy(store: &AuthStore, idx: usize, case: &Case) {
    let id = format!("conformance-{idx}");
    let body = format!(
        r#"{{
            "id":"{id}",
            "version":1,
            "statements":{}
        }}"#,
        case.statements
    );
    let policy = reddb::auth::policies::Policy::from_json_str(&body).unwrap();
    store.put_policy(policy).unwrap();
    let user = match case.principal {
        Principal::PlatformAlice => UserId::platform("alice"),
        Principal::TenantAlice(tenant) => UserId::from_parts(Some(tenant), "alice"),
    };
    store
        .attach_policy(reddb::auth::store::PrincipalRef::User(user), &id)
        .unwrap();
}

fn setup_fixture(rt: &RedDBRuntime, fixture: Fixture) {
    match fixture {
        Fixture::Users => {
            rt.execute_query("CREATE TABLE users (id INT, name TEXT, email TEXT)")
                .unwrap();
            rt.execute_query(
                "INSERT INTO users (id, name, email) VALUES (1, 'Ada', 'a@example.com')",
            )
            .unwrap();
        }
        Fixture::UsersJoinOrders => {
            rt.execute_query("CREATE TABLE users (id INT, name TEXT, email TEXT)")
                .unwrap();
            rt.execute_query("CREATE TABLE orders (id INT, user_id INT, total INT)")
                .unwrap();
            rt.execute_query(
                "INSERT INTO users (id, name, email) VALUES (1, 'Ada', 'a@example.com')",
            )
            .unwrap();
            rt.execute_query("INSERT INTO orders (id, user_id, total) VALUES (10, 1, 42)")
                .unwrap();
        }
        Fixture::Accounts => {
            rt.execute_query("CREATE TABLE accounts (id INT, status TEXT, secret TEXT)")
                .unwrap();
            rt.execute_query("INSERT INTO accounts (id, status, secret) VALUES (1, 'old', 's1')")
                .unwrap();
        }
        Fixture::Orders => {
            rt.execute_query("CREATE TABLE orders (id INT, public TEXT, note TEXT, secret TEXT)")
                .unwrap();
        }
        Fixture::TenantEvents => {
            rt.execute_query("CREATE TABLE events (id INT, tenant_id TEXT) TENANT BY (tenant_id)")
                .unwrap();
        }
        Fixture::Docs => {
            rt.execute_query(
                r#"INSERT INTO docs DOCUMENT (body) VALUES ('{"public":"ok","secret":"no","nested":{"public":"yes","secret":"hidden"}}')"#,
            )
            .unwrap();
        }
        Fixture::Embeddings => {
            rt.execute_query(
                "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'secret')",
            )
            .unwrap();
        }
        Fixture::SocialGraph => {
            rt.execute_query(
                "INSERT INTO social NODE (label, name, secret) VALUES ('User', 'alice', 'pii')",
            )
            .unwrap();
        }
        Fixture::Metrics => {
            rt.execute_query("CREATE TIMESERIES metrics RETENTION 7 d")
                .unwrap();
            rt.execute_query(
                "INSERT INTO metrics (metric, value, tags, timestamp) VALUES \
                 ('cpu', 50.0, {tenant: 'acme', host: 'a1'}, 1704067200000000000)",
            )
            .unwrap();
        }
    }
}

fn assert_case(idx: usize, case: &Case) {
    let (rt, store) = runtime_with_auth();
    setup_fixture(&rt, case.fixture);
    attach_case_policy(&store, idx, case);

    let result = as_principal(case.principal, || rt.execute_query(case.sql));
    match (case.expected, result) {
        (Expected::Allow, Ok(_)) => {}
        (Expected::Allow, Err(err)) => {
            panic!(
                "case `{}` should allow {} {:?} path `{}` via `{}`; got {err:?}",
                case.name, case.action, case.model, case.path, case.sql
            );
        }
        (Expected::Deny { resource }, Ok(ok)) => {
            panic!(
                "case `{}` should deny {resource} for {} {:?} path `{}` via `{}`; got {ok:?}",
                case.name, case.action, case.model, case.path, case.sql
            );
        }
        (Expected::Deny { resource }, Err(err)) => {
            assert_error_names_resource(case, resource, err);
        }
    }
}

fn assert_error_names_resource(case: &Case, resource: &str, err: RedDBError) {
    let msg = err.to_string();
    let names_resource = msg.contains(resource)
        || resource
            .strip_prefix("column:")
            .is_some_and(|column_name| msg.contains(column_name));
    assert!(
        names_resource,
        "case `{}` expected denial for {resource}; got {msg}",
        case.name
    );
}

const TABLE_USERS_WITH_EMAIL_DENY: &str = r#"[
    {"effect":"allow","actions":["select"],"resources":["table:users"]},
    {"effect":"deny","actions":["select"],"resources":["column:users.email"]}
]"#;

const JOIN_WITH_EMAIL_DENY: &str = r#"[
    {"effect":"allow","actions":["select"],"resources":["database:*"]},
    {"effect":"allow","actions":["select"],"resources":["table:users"]},
    {"effect":"allow","actions":["select"],"resources":["table:orders"]},
    {"effect":"deny","actions":["select"],"resources":["column:users.email"]}
]"#;

const DOCS_DENY_SECRET_PATHS: &str = r#"[
    {"effect":"allow","actions":["select"],"resources":["table:docs"]},
    {"effect":"deny","actions":["select"],"resources":["column:docs.body.secret"]},
    {"effect":"deny","actions":["select"],"resources":["column:docs.body.nested.secret"]}
]"#;

const ACCOUNTS_UPDATE_TABLE_ALLOW: &str = r#"[
    {"effect":"allow","actions":["select","update"],"resources":["table:accounts"]}
]"#;

const ACCOUNTS_UPDATE_SECRET_DENY: &str = r#"[
    {"effect":"allow","actions":["select","update"],"resources":["table:accounts"]},
    {"effect":"deny","actions":["update"],"resources":["column:accounts.secret"]}
]"#;

const ORDERS_INSERT_SECRET_DENY: &str = r#"[
    {"effect":"allow","actions":["insert"],"resources":["table:orders"]},
    {"effect":"deny","actions":["insert"],"resources":["column:orders.secret"]}
]"#;

const CORPUS: &[Case] = &[
    Case {
        name: "relational-id-projection-allowed",
        model: Model::Relational,
        action: "select",
        path: "users.id",
        fixture: Fixture::Users,
        principal: Principal::PlatformAlice,
        statements: TABLE_USERS_WITH_EMAIL_DENY,
        sql: "SELECT id FROM users",
        expected: Expected::Allow,
    },
    Case {
        name: "relational-name-projection-allowed",
        model: Model::Relational,
        action: "select",
        path: "users.name",
        fixture: Fixture::Users,
        principal: Principal::PlatformAlice,
        statements: TABLE_USERS_WITH_EMAIL_DENY,
        sql: "SELECT name FROM users",
        expected: Expected::Allow,
    },
    Case {
        name: "relational-explicit-email-denied",
        model: Model::Relational,
        action: "select",
        path: "users.email",
        fixture: Fixture::Users,
        principal: Principal::PlatformAlice,
        statements: TABLE_USERS_WITH_EMAIL_DENY,
        sql: "SELECT email FROM users",
        expected: Expected::Deny {
            resource: "column:users.email",
        },
    },
    Case {
        name: "relational-wildcard-sees-denied-email",
        model: Model::Relational,
        action: "select",
        path: "users.*",
        fixture: Fixture::Users,
        principal: Principal::PlatformAlice,
        statements: TABLE_USERS_WITH_EMAIL_DENY,
        sql: "SELECT * FROM users",
        expected: Expected::Deny {
            resource: "column:users.email",
        },
    },
    Case {
        name: "relational-column-allow-without-table-denied",
        model: Model::Relational,
        action: "select",
        path: "users.id",
        fixture: Fixture::Users,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["select"],"resources":["column:users.id"]}
        ]"#,
        sql: "SELECT id FROM users",
        expected: Expected::Deny {
            resource: "table:users",
        },
    },
    Case {
        name: "relational-join-safe-columns-allowed",
        model: Model::Relational,
        action: "select",
        path: "users.name + orders.total",
        fixture: Fixture::UsersJoinOrders,
        principal: Principal::PlatformAlice,
        statements: JOIN_WITH_EMAIL_DENY,
        sql: "FROM users u JOIN orders o ON u.id = o.user_id RETURN u.name, o.total",
        expected: Expected::Allow,
    },
    Case {
        name: "relational-join-denied-aliased-email",
        model: Model::Relational,
        action: "select",
        path: "users.email",
        fixture: Fixture::UsersJoinOrders,
        principal: Principal::PlatformAlice,
        statements: JOIN_WITH_EMAIL_DENY,
        sql: "FROM users u JOIN orders o ON u.id = o.user_id RETURN u.email, o.total",
        expected: Expected::Deny {
            resource: "column:users.email",
        },
    },
    Case {
        name: "document-public-path-allowed",
        model: Model::Document,
        action: "select",
        path: "docs.body.public",
        fixture: Fixture::Docs,
        principal: Principal::PlatformAlice,
        statements: DOCS_DENY_SECRET_PATHS,
        sql: "SELECT body.public FROM docs",
        expected: Expected::Allow,
    },
    Case {
        name: "document-secret-path-denied",
        model: Model::Document,
        action: "select",
        path: "docs.body.secret",
        fixture: Fixture::Docs,
        principal: Principal::PlatformAlice,
        statements: DOCS_DENY_SECRET_PATHS,
        sql: "SELECT body.secret FROM docs",
        expected: Expected::Deny {
            resource: "column:docs.body.secret",
        },
    },
    Case {
        name: "document-nested-public-path-allowed",
        model: Model::Document,
        action: "select",
        path: "docs.body.nested.public",
        fixture: Fixture::Docs,
        principal: Principal::PlatformAlice,
        statements: DOCS_DENY_SECRET_PATHS,
        sql: "SELECT body.nested.public FROM docs",
        expected: Expected::Allow,
    },
    Case {
        name: "document-nested-secret-path-denied",
        model: Model::Document,
        action: "select",
        path: "docs.body.nested.secret",
        fixture: Fixture::Docs,
        principal: Principal::PlatformAlice,
        statements: DOCS_DENY_SECRET_PATHS,
        sql: "SELECT body.nested.secret FROM docs",
        expected: Expected::Deny {
            resource: "column:docs.body.nested.secret",
        },
    },
    Case {
        name: "document-base-body-column-denied",
        model: Model::Document,
        action: "select",
        path: "docs.body",
        fixture: Fixture::Docs,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["select"],"resources":["table:docs"]},
            {"effect":"deny","actions":["select"],"resources":["column:docs.body"]}
        ]"#,
        sql: "SELECT body FROM docs",
        expected: Expected::Deny {
            resource: "column:docs.body",
        },
    },
    Case {
        name: "document-wildcard-column-denied",
        model: Model::Document,
        action: "select",
        path: "docs.*",
        fixture: Fixture::Docs,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["select"],"resources":["table:docs"]},
            {"effect":"deny","actions":["select"],"resources":["column:docs.*"]}
        ]"#,
        sql: "SELECT * FROM docs",
        expected: Expected::Deny {
            resource: "column:docs.*",
        },
    },
    Case {
        name: "update-status-allowed",
        model: Model::Relational,
        action: "update",
        path: "accounts.status",
        fixture: Fixture::Accounts,
        principal: Principal::PlatformAlice,
        statements: ACCOUNTS_UPDATE_TABLE_ALLOW,
        sql: "UPDATE accounts SET status = 'active' WHERE id = 1",
        expected: Expected::Allow,
    },
    Case {
        name: "update-id-allowed",
        model: Model::Relational,
        action: "update",
        path: "accounts.id",
        fixture: Fixture::Accounts,
        principal: Principal::PlatformAlice,
        statements: ACCOUNTS_UPDATE_TABLE_ALLOW,
        sql: "UPDATE accounts SET id = 2 WHERE id = 1",
        expected: Expected::Allow,
    },
    Case {
        name: "update-secret-denied",
        model: Model::Relational,
        action: "update",
        path: "accounts.secret",
        fixture: Fixture::Accounts,
        principal: Principal::PlatformAlice,
        statements: ACCOUNTS_UPDATE_SECRET_DENY,
        sql: "UPDATE accounts SET secret = 's2' WHERE id = 1",
        expected: Expected::Deny {
            resource: "column:accounts.secret",
        },
    },
    Case {
        name: "update-mixed-set-denied",
        model: Model::Relational,
        action: "update",
        path: "accounts.status + accounts.secret",
        fixture: Fixture::Accounts,
        principal: Principal::PlatformAlice,
        statements: ACCOUNTS_UPDATE_SECRET_DENY,
        sql: "UPDATE accounts SET status = 'active', secret = 's2' WHERE id = 1",
        expected: Expected::Deny {
            resource: "column:accounts.secret",
        },
    },
    Case {
        name: "update-column-allow-without-table-denied",
        model: Model::Relational,
        action: "update",
        path: "accounts.status",
        fixture: Fixture::Accounts,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["update"],"resources":["column:accounts.status"]}
        ]"#,
        sql: "UPDATE accounts SET status = 'active' WHERE id = 1",
        expected: Expected::Deny {
            resource: "table:accounts",
        },
    },
    Case {
        name: "insert-public-note-allowed",
        model: Model::Relational,
        action: "insert",
        path: "orders.id + orders.note",
        fixture: Fixture::Orders,
        principal: Principal::PlatformAlice,
        statements: ORDERS_INSERT_SECRET_DENY,
        sql: "INSERT INTO orders (id, note) VALUES (1, 'ok')",
        expected: Expected::Allow,
    },
    Case {
        name: "insert-public-column-allowed",
        model: Model::Relational,
        action: "insert",
        path: "orders.public",
        fixture: Fixture::Orders,
        principal: Principal::PlatformAlice,
        statements: ORDERS_INSERT_SECRET_DENY,
        sql: "INSERT INTO orders (id, public) VALUES (1, 'ok')",
        expected: Expected::Allow,
    },
    Case {
        name: "insert-secret-column-denied",
        model: Model::Relational,
        action: "insert",
        path: "orders.secret",
        fixture: Fixture::Orders,
        principal: Principal::PlatformAlice,
        statements: ORDERS_INSERT_SECRET_DENY,
        sql: "INSERT INTO orders (id, secret) VALUES (1, 'nope')",
        expected: Expected::Deny {
            resource: "column:orders.secret",
        },
    },
    Case {
        name: "insert-mixed-column-list-denied",
        model: Model::Relational,
        action: "insert",
        path: "orders.public + orders.secret",
        fixture: Fixture::Orders,
        principal: Principal::PlatformAlice,
        statements: ORDERS_INSERT_SECRET_DENY,
        sql: "INSERT INTO orders (id, public, secret) VALUES (1, 'ok', 'nope')",
        expected: Expected::Deny {
            resource: "column:orders.secret",
        },
    },
    Case {
        name: "insert-multi-row-allowed",
        model: Model::Relational,
        action: "insert",
        path: "orders.id + orders.note",
        fixture: Fixture::Orders,
        principal: Principal::PlatformAlice,
        statements: ORDERS_INSERT_SECRET_DENY,
        sql: "INSERT INTO orders (id, note) VALUES (1, 'a'), (2, 'b')",
        expected: Expected::Allow,
    },
    Case {
        name: "insert-column-allow-without-table-denied",
        model: Model::Relational,
        action: "insert",
        path: "orders.id",
        fixture: Fixture::Orders,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["insert"],"resources":["column:orders.id"]}
        ]"#,
        sql: "INSERT INTO orders (id) VALUES (1)",
        expected: Expected::Deny {
            resource: "table:orders",
        },
    },
    Case {
        name: "insert-tenant-auto-fill-denied",
        model: Model::Relational,
        action: "insert",
        path: "events.tenant_id",
        fixture: Fixture::TenantEvents,
        principal: Principal::TenantAlice("acme"),
        statements: r#"[
            {"effect":"allow","actions":["insert"],"resources":["table:tenant/acme/events"]},
            {"effect":"deny","actions":["insert"],"resources":["column:tenant/acme/events.tenant_id"]}
        ]"#,
        sql: "INSERT INTO events (id) VALUES (1)",
        expected: Expected::Deny {
            resource: "column:events.tenant_id",
        },
    },
    Case {
        name: "vector-search-content-allowed",
        model: Model::Vector,
        action: "select",
        path: "embeddings.content",
        fixture: Fixture::Embeddings,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["select"],"resources":["table:embeddings"]}
        ]"#,
        sql: "VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 1",
        expected: Expected::Allow,
    },
    Case {
        name: "vector-search-content-denied",
        model: Model::Vector,
        action: "select",
        path: "embeddings.content",
        fixture: Fixture::Embeddings,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["select"],"resources":["table:embeddings"]},
            {"effect":"deny","actions":["select"],"resources":["column:embeddings.content"]}
        ]"#,
        sql: "VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 1",
        expected: Expected::Deny {
            resource: "column:embeddings.content",
        },
    },
    Case {
        name: "graph-return-name-allowed",
        model: Model::Graph,
        action: "select",
        path: "graph.name",
        fixture: Fixture::SocialGraph,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["select"],"resources":["table:graph"]},
            {"effect":"deny","actions":["select"],"resources":["column:graph.secret"]}
        ]"#,
        sql: "MATCH (n:User) RETURN n.name",
        expected: Expected::Allow,
    },
    Case {
        name: "graph-return-secret-denied",
        model: Model::Graph,
        action: "select",
        path: "graph.secret",
        fixture: Fixture::SocialGraph,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["select"],"resources":["table:graph"]},
            {"effect":"deny","actions":["select"],"resources":["column:graph.secret"]}
        ]"#,
        sql: "MATCH (n:User) RETURN n.secret",
        expected: Expected::Deny {
            resource: "column:graph.secret",
        },
    },
    Case {
        name: "timeseries-select-value-allowed",
        model: Model::Timeseries,
        action: "select",
        path: "metrics.value",
        fixture: Fixture::Metrics,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["select"],"resources":["table:metrics"]},
            {"effect":"deny","actions":["select"],"resources":["column:metrics.tags"]}
        ]"#,
        sql: "SELECT value FROM metrics",
        expected: Expected::Allow,
    },
    Case {
        name: "timeseries-select-tags-denied",
        model: Model::Timeseries,
        action: "select",
        path: "metrics.tags",
        fixture: Fixture::Metrics,
        principal: Principal::PlatformAlice,
        statements: r#"[
            {"effect":"allow","actions":["select"],"resources":["table:metrics"]},
            {"effect":"deny","actions":["select"],"resources":["column:metrics.tags"]}
        ]"#,
        sql: "SELECT tags FROM metrics",
        expected: Expected::Deny {
            resource: "column:metrics.tags",
        },
    },
];

#[test]
fn column_policy_conformance_corpus_covers_required_surface() {
    assert!(
        CORPUS.len() >= 30,
        "issue #271 requires at least 30 path x model x action cases"
    );

    let models = CORPUS
        .iter()
        .map(|case| case.model)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        models,
        BTreeSet::from([
            Model::Relational,
            Model::Document,
            Model::Vector,
            Model::Graph,
            Model::Timeseries,
        ])
    );
    for required_action in ["select", "update", "insert"] {
        assert!(
            CORPUS.iter().any(|case| case.action == required_action),
            "missing action coverage for {required_action}"
        );
    }

    for (idx, case) in CORPUS.iter().enumerate() {
        assert_case(idx, case);
    }
}
