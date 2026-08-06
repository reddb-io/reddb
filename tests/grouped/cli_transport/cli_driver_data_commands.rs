//! Driver-backed `red` data-command contract (issue #2124).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use reddb_client::{format_query_result, QueryResult, Reddb, RowFormat};

use crate::support;

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn run_red_query(path: &Path, format: &str) -> Vec<u8> {
    let output = Command::new(red_binary())
        .args([
            "query",
            "--path",
            &path.display().to_string(),
            "SELECT id, name FROM people ORDER BY id",
            "--format",
            format,
        ])
        .output()
        .expect("spawn red query");
    assert!(
        output.status.success(),
        "red query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn query_output_matches_driver_row_format_goldens() {
    let path = support::temp_db_file("cli-driver-row-format");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let expected = runtime.block_on(async {
        let db = Reddb::connect(&file_uri(&path))
            .await
            .expect("connect driver");
        db.query("CREATE TABLE people (id INTEGER, name TEXT)")
            .await
            .expect("create table");
        db.query("INSERT INTO people (id, name) VALUES (1, 'Ada'), (2, 'Linus')")
            .await
            .expect("insert rows");
        let result = db
            .query("SELECT id, name FROM people ORDER BY id")
            .await
            .expect("query rows");
        db.close().await.expect("close driver");
        [
            ("table", format_query_result(&result, RowFormat::Table)),
            ("json", format_query_result(&result, RowFormat::Json)),
            ("toon", format_query_result(&result, RowFormat::Toon)),
        ]
    });

    assert_eq!(expected[0].1, b"id  name\n--  -----\n1   Ada\n2   Linus\n");
    assert_eq!(
        expected[1].1,
        br#"[{"id":1,"name":"Ada"},{"id":2,"name":"Linus"}]
"#
    );
    assert_eq!(expected[2].1, b"[2]{id,name}:\n  1,Ada\n  2,Linus\n");

    for (format, driver_output) in expected {
        assert_eq!(
            run_red_query(&path, format),
            driver_output,
            "format {format}"
        );
    }
}

#[test]
fn explicit_bind_routes_query_through_the_driver_http_adapter() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock HTTP server");
    let address = listener.local_addr().expect("mock HTTP address");
    let server = thread::spawn(move || {
        for (expected_request, body) in [
            ("GET /health ", r#"{"ok":true,"status":"ok"}"#),
            (
                "POST /query ",
                r#"{"ok":true,"result":{"statement":"select","affected":0,"columns":["source"],"rows":[{"source":"driver-http"}]}}"#,
            ),
        ] {
            let (mut stream, _) = listener.accept().expect("accept HTTP request");
            let mut request = [0u8; 4096];
            let count = stream.read(&mut request).expect("read HTTP request");
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(
                request.starts_with(expected_request),
                "unexpected request: {request}"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write HTTP response");
        }
    });

    let output = Command::new(red_binary())
        .args([
            "query",
            "--bind",
            &address.to_string(),
            "SELECT source FROM remote_fixture",
            "--format",
            "json",
        ])
        .output()
        .expect("spawn red query");
    assert!(
        output.status.success(),
        "red query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        br#"[{"source":"driver-http"}]
"#
    );
    server.join().expect("mock HTTP server");
}

#[test]
fn migrated_data_command_slice_has_no_raw_http_client() {
    let source = include_str!("../../../src/bin/red.rs");
    let (_, migrated_and_after) = source
        .split_once("// DATA COMMANDS BEGIN (issue #2124)")
        .expect("data-command source boundary");
    let (migrated, _) = migrated_and_after
        .split_once("// DATA COMMANDS END (issue #2124)")
        .expect("data-command source boundary");

    assert!(migrated.contains("reddb_client::Reddb"));
    assert!(!migrated.contains("TcpStream"));
    assert!(!migrated.contains("post_json_to_http"));
    assert!(!migrated.contains("get_from_http"));
}

#[test]
fn cli_binary_has_no_parallel_http_value_or_flag_schema_implementation() {
    let source = include_str!("../../../src/bin/red.rs");

    for retired in [
        "post_json_to_http",
        "get_from_http",
        "base64_encode",
        "schema_value_to_value_out",
        "AdminQueryTable",
        "format_admin_table",
        "format_admin_csv",
        "fn build_flags_for_command",
        "POST {path} HTTP/1.1",
        "GET {path} HTTP/1.1",
    ] {
        assert!(
            !source.contains(retired),
            "red binary still contains retired parallel-client symbol {retired}"
        );
    }

    let cli_commands = include_str!("../../../crates/reddb-server/src/cli/commands.rs");
    assert!(cli_commands.contains("pub fn flags_for_command"));
}

#[test]
fn admin_query_output_matches_driver_row_format_golden() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock HTTP server");
    let address = listener.local_addr().expect("mock HTTP address");
    let server = thread::spawn(move || {
        for (expected_request, body) in [
            ("GET /health ", r#"{"ok":true,"status":"ok"}"#),
            (
                "POST /query ",
                r#"{"ok":true,"result":{"statement":"select","affected":0,"columns":["kind","value"],"rows":[{"kind":"null","value":null},{"kind":"bool","value":true},{"kind":"integer","value":42},{"kind":"float","value":1.5},{"kind":"text","value":"line\nquote\""}]}}"#,
            ),
        ] {
            let (mut stream, _) = listener.accept().expect("accept HTTP request");
            let mut request = [0u8; 4096];
            let count = stream.read(&mut request).expect("read HTTP request");
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(
                request.starts_with(expected_request),
                "unexpected request: {request}"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write HTTP response");
        }
    });

    let output = Command::new(red_binary())
        .args([
            "admin",
            "query",
            "SELECT kind, value FROM fixture",
            "--bind",
            &address.to_string(),
            "--json",
        ])
        .output()
        .expect("spawn red admin query");
    assert!(
        output.status.success(),
        "red admin query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let driver_result = QueryResult {
        statement: "select".to_string(),
        affected: 0,
        columns: vec!["kind".to_string(), "value".to_string()],
        rows: vec![
            vec![
                (
                    "kind".to_string(),
                    reddb_client::ValueOut::String("null".to_string()),
                ),
                ("value".to_string(), reddb_client::ValueOut::Null),
            ],
            vec![
                (
                    "kind".to_string(),
                    reddb_client::ValueOut::String("bool".to_string()),
                ),
                ("value".to_string(), reddb_client::ValueOut::Bool(true)),
            ],
            vec![
                (
                    "kind".to_string(),
                    reddb_client::ValueOut::String("integer".to_string()),
                ),
                ("value".to_string(), reddb_client::ValueOut::Integer(42)),
            ],
            vec![
                (
                    "kind".to_string(),
                    reddb_client::ValueOut::String("float".to_string()),
                ),
                ("value".to_string(), reddb_client::ValueOut::Float(1.5)),
            ],
            vec![
                (
                    "kind".to_string(),
                    reddb_client::ValueOut::String("text".to_string()),
                ),
                (
                    "value".to_string(),
                    reddb_client::ValueOut::String("line\nquote\"".to_string()),
                ),
            ],
        ],
        notice: None,
    };
    assert_eq!(
        output.stdout,
        format_query_result(&driver_result, RowFormat::Json)
    );
    server.join().expect("mock HTTP server");
}
