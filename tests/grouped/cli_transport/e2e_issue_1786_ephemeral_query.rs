//! End-to-end CLI tests for the ephemeral-store tracer (issues #1786, #1792).
//!
//! Spawns the real `red` binary via `CARGO_BIN_EXE_red` so the full
//! `main()` path is exercised: `red query <file.csv|file.tsv> <sql>`
//! materializes the file into a throwaway in-memory store, runs the
//! query, prints the result, and leaves nothing durable behind.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

fn run_query(file: &str, sql: &str, extra: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(red_binary());
    cmd.arg("query").arg(file).arg(sql);
    for a in extra {
        cmd.arg(a);
    }
    let out = cmd.output().expect("spawn red query");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_query_files(files: &[&str], sql: &str, extra: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(red_binary());
    cmd.arg("query");
    for file in files {
        cmd.arg(file);
    }
    cmd.arg(sql);
    for a in extra {
        cmd.arg(a);
    }
    let out = cmd.output().expect("spawn red query");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-cli-ephemeral-{label}-"))
        .tempdir()
        .expect("temp dir")
}

#[test]
fn red_query_csv_file_no_server_no_store() {
    let dir = temp_dir("csv");
    let path = dir.path().join("people.csv");
    fs::write(&path, "id,name,age\n1,Alice,30\n2,Bob,9\n").expect("write fixture");
    let path_str = path.display().to_string();

    // Query by the positional alias, JSON so the assertion is byte-stable.
    let (code, stdout, stderr) = run_query(&path_str, "SELECT count(*) AS n FROM t", &["--json"]);
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    assert!(stdout.contains("\"ok\":true"), "stdout: {stdout}");
    assert!(
        stdout.contains("\"n\":2"),
        "expected count 2; stdout: {stdout}"
    );

    // Query by the sanitized file-stem name, numeric filter proves typed
    // columns (only the age-30 row survives `age > 26`).
    let (code, stdout, stderr) = run_query(
        &path_str,
        "SELECT name FROM people WHERE age > 26",
        &["--json"],
    );
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    assert!(stdout.contains("\"Alice\""), "stdout: {stdout}");
    assert!(!stdout.contains("\"Bob\""), "Bob leaked: {stdout}");

    // The ephemeral store leaves no durable artifacts next to the file.
    let mut names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["people.csv".to_string()]);
}

#[test]
fn red_query_tsv_file_by_alias() {
    let dir = temp_dir("tsv");
    let path = dir.path().join("places.tsv");
    fs::write(&path, "id\tcity\n1\tLisbon\n2\tPorto\n").expect("write fixture");
    let path_str = path.display().to_string();

    let (code, stdout, stderr) = run_query(
        &path_str,
        "SELECT city FROM t WHERE city = 'Porto'",
        &["--json"],
    );
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    assert!(stdout.contains("\"Porto\""), "stdout: {stdout}");
    assert!(!stdout.contains("\"Lisbon\""), "Lisbon leaked: {stdout}");
}

#[test]
fn red_query_multiple_files_join_by_alias_and_stem() {
    let dir = temp_dir("multi");
    let users = dir.path().join("users.csv");
    let orders = dir.path().join("orders.csv");
    fs::write(&users, "id,name\n1,Alice\n2,Bob\n").expect("write users");
    fs::write(&orders, "id,user_id,total\n10,1,25\n11,2,7\n").expect("write orders");
    let users_str = users.display().to_string();
    let orders_str = orders.display().to_string();

    let (code, stdout, stderr) = run_query_files(
        &[&users_str, &orders_str],
        "SELECT t1.name, t2.total FROM t1 JOIN t2 ON t1.id = t2.user_id WHERE t2.total > 10",
        &["--json"],
    );
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    assert!(stdout.contains("\"Alice\""), "stdout: {stdout}");
    assert!(stdout.contains("\"t2.total\":25"), "stdout: {stdout}");
    assert!(!stdout.contains("\"Bob\""), "Bob leaked: {stdout}");

    let (code, stdout, stderr) = run_query_files(
        &[&users_str, &orders_str],
        "SELECT users.name, orders.total FROM users JOIN orders ON users.id = orders.user_id WHERE orders.total > 10",
        &["--json"],
    );
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    assert!(
        stdout.contains("\"users.name\":\"Alice\""),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("\"orders.total\":25"), "stdout: {stdout}");

    let (code, stdout, stderr) =
        run_query_files(&[&users_str, &orders_str], "SHOW STATS t1", &["--json"]);
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    assert!(stdout.contains("\"collection\":\"t1\""), "stdout: {stdout}");
    assert!(
        stdout.contains("\"metric\":\"row_count\""),
        "stdout: {stdout}"
    );

    let (code, stdout, stderr) =
        run_query_files(&[&users_str, &orders_str], "SHOW STATS users", &["--json"]);
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    assert!(
        stdout.contains("\"collection\":\"users\""),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"metric\":\"row_count\""),
        "stdout: {stdout}"
    );
}

#[test]
fn red_query_multiple_files_collision_uses_deterministic_stems_and_aliases() {
    let dir = temp_dir("collision");
    let plain = dir.path().join("items.csv");
    let punctuated = dir.path().join("items!!.csv");
    fs::write(&plain, "id,label\n1,first\n").expect("write plain");
    fs::write(&punctuated, "id,label\n1,second\n").expect("write punctuated");
    let plain_str = plain.display().to_string();
    let punctuated_str = punctuated.display().to_string();

    let (code, stdout, stderr) = run_query_files(
        &[&plain_str, &punctuated_str],
        "SELECT t1.label, t2.label FROM t1 JOIN t2 ON t1.id = t2.id",
        &["--json"],
    );
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    assert!(stdout.contains("\"first\""), "stdout: {stdout}");
    assert!(stdout.contains("\"second\""), "stdout: {stdout}");

    let (code, stdout, stderr) = run_query_files(
        &[&plain_str, &punctuated_str],
        "SELECT items.label, items_2.label FROM items JOIN items_2 ON items.id = items_2.id",
        &["--json"],
    );
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    assert!(
        stdout.contains("\"items.label\":\"first\""),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"items_2.label\":\"second\""),
        "stdout: {stdout}"
    );
}

#[test]
fn red_query_missing_file_errors_didactically() {
    let dir = temp_dir("missing");
    let path = dir.path().join("nope.csv");
    let path_str = path.display().to_string();

    let (code, _stdout, stderr) = run_query(&path_str, "SELECT * FROM t", &[]);
    assert_ne!(code, 0, "missing file should fail");
    assert!(
        stderr.contains("no such file"),
        "expected didactic error; stderr: {stderr}"
    );
}
