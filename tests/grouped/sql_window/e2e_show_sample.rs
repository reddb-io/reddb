use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn row_count(rt: &RedDBRuntime, sql: &str) -> usize {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .result
        .records
        .len()
}

fn seed_users(rt: &RedDBRuntime) {
    exec(rt, "CREATE TABLE users (id INT, name TEXT)");

    for id in 1..=12 {
        exec(
            rt,
            &format!("INSERT INTO users (id, name) VALUES ({id}, 'u{id}')"),
        );
    }
}

#[test]
fn show_sample_uses_default_limit_10() {
    let rt = open_runtime();
    seed_users(&rt);

    assert_eq!(row_count(&rt, "SHOW SAMPLE users"), 10);
}

#[test]
fn show_sample_accepts_explicit_limit() {
    let rt = open_runtime();
    seed_users(&rt);

    assert_eq!(row_count(&rt, "SHOW SAMPLE users LIMIT 5"), 5);
}

#[test]
fn show_sample_uses_normal_select_tenant_filter() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE users (id INT, tenant_id TEXT) TENANT BY (tenant_id)",
    );
    exec(
        &rt,
        "INSERT INTO users (id, tenant_id) VALUES \
         (1, 'acme'), (2, 'acme'), (3, 'globex'), (4, 'globex')",
    );

    exec(&rt, "SET TENANT 'acme'");
    assert_eq!(row_count(&rt, "SHOW SAMPLE users LIMIT 10"), 2);
}

#[test]
fn show_sample_missing_collection_reports_select_path_error() {
    let rt = open_runtime();

    let err = rt
        .execute_query("SHOW SAMPLE missing_users")
        .expect_err("SHOW SAMPLE should fail for missing collections");
    assert!(
        err.to_string().contains("missing_users"),
        "unexpected error: {err:?}"
    );
}
