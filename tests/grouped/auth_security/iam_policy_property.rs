//! Property tests for IAM column-policy enforcement.
//!
//! These exercise the runtime SQL path with randomized column allow/deny
//! statements and randomized SELECT projections. The invariant is simple:
//! a denied projected column must never produce a successful result, and a
//! successful result must never expose denied columns.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};
use serde_json::json;

const TABLE: &str = "users";
const COLUMNS: [&str; 3] = ["id", "name", "email"];
const CASES: u32 = 32;

#[derive(Debug, Clone)]
struct SelectCase {
    allow_columns: BTreeSet<usize>,
    deny_columns: BTreeSet<usize>,
    projection: Projection,
}

#[derive(Debug, Clone)]
enum Projection {
    All,
    Columns(BTreeSet<usize>),
}

fn runtime_with_auth() -> (RedDBRuntime, Arc<AuthStore>) {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    rt.set_auth_store(Arc::clone(&store));
    (rt, store)
}

fn as_user<T>(name: &str, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), Role::Write);
    let out = f();
    clear_current_auth_identity();
    out
}

fn setup_users_table(rt: &RedDBRuntime) {
    rt.execute_query("CREATE TABLE users (id INT, name TEXT, email TEXT)")
        .unwrap();
    rt.execute_query("INSERT INTO users (id, name, email) VALUES (1, 'Ada', 'ada@example.com')")
        .unwrap();
}

fn attach_policy(store: &AuthStore, user: &str, policy_id: &str, case: &SelectCase) {
    let mut statements = vec![json!({
        "effect": "allow",
        "actions": ["select"],
        "resources": [format!("table:{TABLE}")]
    })];

    if !case.allow_columns.is_empty() {
        statements.push(json!({
            "effect": "allow",
            "actions": ["select"],
            "resources": resources_for(&case.allow_columns)
        }));
    }

    if !case.deny_columns.is_empty() {
        statements.push(json!({
            "effect": "deny",
            "actions": ["select"],
            "resources": resources_for(&case.deny_columns)
        }));
    }

    let policy_json = json!({
        "id": policy_id,
        "version": 1,
        "statements": statements
    })
    .to_string();
    let policy = reddb::auth::policies::Policy::from_json_str(&policy_json)
        .unwrap_or_else(|err| panic!("policy parse failed: {err}; policy={policy_json}"));
    store.put_policy(policy).unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform(user)),
            policy_id,
        )
        .unwrap();
}

fn resources_for(columns: &BTreeSet<usize>) -> Vec<String> {
    columns
        .iter()
        .map(|index| format!("column:{TABLE}.{}", COLUMNS[*index]))
        .collect()
}

fn projected_column_indices(projection: &Projection) -> BTreeSet<usize> {
    match projection {
        Projection::All => (0..COLUMNS.len()).collect(),
        Projection::Columns(columns) => columns.clone(),
    }
}

fn select_sql(projection: &Projection) -> String {
    match projection {
        Projection::All => format!("SELECT * FROM {TABLE}"),
        Projection::Columns(columns) => {
            let names: Vec<&str> = columns.iter().map(|index| COLUMNS[*index]).collect();
            format!("SELECT {} FROM {TABLE}", names.join(", "))
        }
    }
}

fn column_set(mask: Vec<bool>) -> BTreeSet<usize> {
    mask.into_iter()
        .enumerate()
        .filter_map(|(index, selected)| selected.then_some(index))
        .collect()
}

fn select_case_strategy() -> impl Strategy<Value = SelectCase> {
    let mask = prop::collection::vec(any::<bool>(), COLUMNS.len());
    (
        mask.clone(),
        mask.clone(),
        prop_oneof![
            Just(Projection::All),
            mask.prop_filter_map("projection must include at least one column", |bits| {
                let columns = column_set(bits);
                (!columns.is_empty()).then_some(Projection::Columns(columns))
            }),
        ],
    )
        .prop_map(|(allow_bits, deny_bits, projection)| SelectCase {
            allow_columns: column_set(allow_bits),
            deny_columns: column_set(deny_bits),
            projection,
        })
}

#[test]
fn denied_select_columns_never_succeed_or_appear() {
    let (rt, store) = runtime_with_auth();
    setup_users_table(&rt);

    let mut runner = TestRunner::new(Config::with_cases(CASES));
    let case_index = Cell::new(0usize);

    runner
        .run(&select_case_strategy(), |case| {
            let next_index = case_index.get() + 1;
            case_index.set(next_index);
            let user = format!("alice_{next_index}");
            let policy_id = format!("property-column-select-{next_index}");
            store.create_user(&user, "p", Role::Write).unwrap();
            attach_policy(&store, &user, &policy_id, &case);

            let projected = projected_column_indices(&case.projection);
            let denied_projection: BTreeSet<usize> = projected
                .intersection(&case.deny_columns)
                .copied()
                .collect();
            let sql = select_sql(&case.projection);

            let result = as_user(&user, || rt.execute_query(&sql));

            if !denied_projection.is_empty() {
                prop_assert!(
                    result.is_err(),
                    "query succeeded despite denied projection: case={case:?}, sql={sql}, result={result:?}"
                );
            } else {
                let result = result.map_err(|err| {
                    TestCaseError::fail(format!(
                        "allowed projection failed: case={case:?}, sql={sql}, err={err:?}"
                    ))
                })?;

                let exposed: BTreeSet<&str> =
                    result.result.columns.iter().map(String::as_str).collect();
                for index in &case.deny_columns {
                    prop_assert!(
                        !exposed.contains(COLUMNS[*index]),
                        "successful query exposed denied column: case={case:?}, sql={sql}, columns={:?}",
                        result.result.columns
                    );
                }
            }
            Ok(())
        })
        .unwrap();
}
