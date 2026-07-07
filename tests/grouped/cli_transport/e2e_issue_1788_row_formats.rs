//! `red query --format` row-output process coverage (issue #1788).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-cli-row-format-{label}-"))
        .tempdir()
        .expect("temp dir")
}

fn run_query(file: &str, format: &str) -> (i32, String, String) {
    let out = Command::new(red_binary())
        .args([
            "query",
            file,
            "SELECT id, name FROM t ORDER BY id",
            "--format",
            format,
        ])
        .output()
        .expect("spawn red query");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn red_query_accepts_each_row_format() {
    let dir = temp_dir("all");
    let path = dir.path().join("people.csv");
    fs::write(&path, "id,name\n1,Ada\n2,Linus\n").expect("write fixture");
    let path = path.display().to_string();

    for (format, expected) in [
        ("table", "id  name\n--  -----\n1   Ada\n2   Linus\n"),
        (
            "json",
            "[{\"id\":1,\"name\":\"Ada\"},{\"id\":2,\"name\":\"Linus\"}]\n",
        ),
        (
            "ndjson",
            "{\"id\":1,\"name\":\"Ada\"}\n{\"id\":2,\"name\":\"Linus\"}\n",
        ),
        ("csv", "id,name\n1,Ada\n2,Linus\n"),
        ("tsv", "id\tname\n1\tAda\n2\tLinus\n"),
        ("toon", "[2]{id,name}:\n  1,Ada\n  2,Linus\n"),
    ] {
        let (code, stdout, stderr) = run_query(&path, format);
        assert_eq!(code, 0, "{format} stderr: {stderr}");
        assert_eq!(stdout, expected, "format {format}");
    }
}

#[test]
fn red_query_defaults_to_table_format() {
    let dir = temp_dir("default");
    let path = dir.path().join("people.csv");
    fs::write(&path, "id,name\n1,Ada\n").expect("write fixture");
    let out = Command::new(red_binary())
        .args([
            "query",
            &path.display().to_string(),
            "SELECT id, name FROM t ORDER BY id",
        ])
        .output()
        .expect("spawn red query");
    assert_eq!(out.status.code().unwrap_or(-1), 0);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "id  name\n--  ----\n1   Ada\n"
    );
}
