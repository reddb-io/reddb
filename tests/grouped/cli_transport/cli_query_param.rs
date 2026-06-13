//! `red query --param` ergonomics (issue #375).
//!
//! Boots the real `red` binary against a persistent scratch DB and pins:
//!   - repeatable `--param` covers `$1`, `$2`, …
//!   - implicit JSON auto-typing (int / string)
//!   - `--param-type` overrides (text)
//!   - `@file` loads JSON content (used for vectors)
//!   - the legacy no-param path still works
//!   - arity errors surface clearly

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn red() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

fn run_query(path: &std::path::Path, sql: &str, extra: &[&str]) -> (String, String, i32) {
    let path_str = path.display().to_string();
    let mut args: Vec<&str> = vec!["query", "--path", &path_str, sql];
    args.extend_from_slice(extra);
    let out = Command::new(red())
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn red");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn seed(path: &std::path::Path) {
    let (_o, e, c) = run_query(path, "CREATE TABLE t (id INTEGER, name TEXT)", &["--json"]);
    assert_eq!(c, 0, "create: {e}");
    for (id, name) in [(1, "alice"), (2, "bob"), (3, "carol")] {
        let sql = format!("INSERT INTO t (id, name) VALUES ({id}, '{name}')");
        let (_o, e, c) = run_query(path, &sql, &["--json"]);
        assert_eq!(c, 0, "insert {id}: {e}");
    }
}

#[test]
fn legacy_no_param_select_still_works() {
    let path = support::temp_db_file("cli-param-legacy");
    seed(&path);
    let (stdout, stderr, code) = run_query(&path, "SELECT * FROM t", &["--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("alice"), "stdout: {stdout}");
}

#[test]
fn single_int_param_binds_dollar_one() {
    let path = support::temp_db_file("cli-param-int");
    seed(&path);
    let (stdout, stderr, code) = run_query(
        &path,
        "SELECT * FROM t WHERE id = $1",
        &["--param", "2", "--json"],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("bob"), "stdout: {stdout}");
    assert!(!stdout.contains("alice"), "stdout: {stdout}");
}

#[test]
fn auto_typed_string_param_falls_back_to_text() {
    let path = support::temp_db_file("cli-param-text");
    seed(&path);
    let (stdout, stderr, code) = run_query(
        &path,
        "SELECT * FROM t WHERE name = $1",
        &["--param", "alice", "--json"],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("alice"), "stdout: {stdout}");
    assert!(!stdout.contains("bob"), "stdout: {stdout}");
}

#[test]
fn multiple_params_bind_positional_order() {
    let path = support::temp_db_file("cli-param-multi");
    seed(&path);
    let (stdout, stderr, code) = run_query(
        &path,
        "SELECT * FROM t WHERE id = $1 AND name = $2",
        &["--param", "1", "--param", "alice", "--json"],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("alice"), "stdout: {stdout}");
    assert!(!stdout.contains("bob"), "stdout: {stdout}");
}

#[test]
fn param_type_text_keeps_numeric_string_as_text() {
    let path = support::temp_db_file("cli-param-ty-text");
    seed(&path);
    // Insert a row whose name is the digit string "42".
    let (_o, e, c) = run_query(
        &path,
        "INSERT INTO t (id, name) VALUES (42, '42')",
        &["--json"],
    );
    assert_eq!(c, 0, "insert: {e}");
    // Force the bind to be Text("42") so the predicate matches `name`.
    let (stdout, stderr, code) = run_query(
        &path,
        "SELECT * FROM t WHERE name = $1",
        &["--param", "42", "--param-type", "text", "--json"],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stdout.contains("\"42\""),
        "expected text bind row: {stdout}"
    );
}

#[test]
fn param_at_file_loads_json_content() {
    let path = support::temp_db_file("cli-param-file");
    seed(&path);
    // Drop the value into a JSON file (`@file` form).
    let pf = path.parent().unwrap().join("val.json");
    std::fs::write(&pf, "1").unwrap();
    let at = format!("@{}", pf.display());
    let (stdout, stderr, code) = run_query(
        &path,
        "SELECT * FROM t WHERE id = $1",
        &["--param", &at, "--json"],
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("alice"), "stdout: {stdout}");
}

#[test]
fn arity_mismatch_is_a_clear_error() {
    let path = support::temp_db_file("cli-param-arity");
    seed(&path);
    let (_stdout, stderr, code) = run_query(
        &path,
        "SELECT * FROM t WHERE id = $1 AND name = $2",
        &["--param", "1"],
    );
    assert_ne!(code, 0, "should fail with arity mismatch");
    assert!(!stderr.is_empty(), "stderr should explain: {stderr}");
}
