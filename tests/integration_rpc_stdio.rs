//! Integration test: spawn the `red` binary in `rpc --stdio` mode and
//! exercise the JSON-RPC 2.0 protocol end-to-end.
//!
//! This is the contract test that drivers in every language must
//! match. If this passes, the wire format is stable.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn red_binary() -> PathBuf {
    // CARGO_BIN_EXE_red is set by Cargo when running integration tests
    // for a crate that has [[bin]] name = "red".
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

struct StdioSession {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl StdioSession {
    fn spawn() -> Self {
        let mut child = Command::new(red_binary())
            .args(["rpc", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn red rpc --stdio");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, request: &str) -> String {
        writeln!(self.stdin, "{request}").expect("write request");
        self.stdin.flush().expect("flush stdin");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        line.trim_end().to_string()
    }

    fn close(mut self) {
        let _ = self.stdin.write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":\"final\",\"method\":\"close\",\"params\":{}}\n",
        );
        let _ = self.stdin.flush();
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

#[test]
fn version_method_returns_protocol_one_zero() {
    let mut s = StdioSession::spawn();
    let resp = s.send(r#"{"jsonrpc":"2.0","id":1,"method":"version","params":{}}"#);
    assert!(resp.contains("\"protocol\":\"1.0\""), "got: {resp}");
    assert!(resp.contains("\"version\""), "got: {resp}");
    assert!(resp.contains("\"id\":1"), "got: {resp}");
    s.close();
}

#[test]
fn health_method_returns_ok_true() {
    let mut s = StdioSession::spawn();
    let resp = s.send(r#"{"jsonrpc":"2.0","id":42,"method":"health","params":{}}"#);
    assert!(resp.contains("\"ok\":true"), "got: {resp}");
    assert!(resp.contains("\"id\":42"), "got: {resp}");
    s.close();
}

#[test]
fn parse_error_on_invalid_json() {
    let mut s = StdioSession::spawn();
    let resp = s.send("this is not json");
    assert!(resp.contains("\"code\":\"PARSE_ERROR\""), "got: {resp}");
    assert!(resp.contains("\"id\":null"), "got: {resp}");
    s.close();
}

#[test]
fn invalid_request_when_method_is_missing() {
    let mut s = StdioSession::spawn();
    let resp = s.send(r#"{"jsonrpc":"2.0","id":1,"params":{}}"#);
    assert!(resp.contains("\"code\":\"INVALID_REQUEST\""), "got: {resp}");
    s.close();
}

#[test]
fn invalid_request_for_unknown_method() {
    let mut s = StdioSession::spawn();
    let resp = s.send(r#"{"jsonrpc":"2.0","id":1,"method":"frobnicate","params":{}}"#);
    assert!(resp.contains("\"code\":\"INVALID_REQUEST\""), "got: {resp}");
    assert!(resp.contains("frobnicate"), "got: {resp}");
    s.close();
}

#[test]
fn invalid_params_when_query_sql_missing() {
    let mut s = StdioSession::spawn();
    let resp = s.send(r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{}}"#);
    assert!(resp.contains("\"code\":\"INVALID_PARAMS\""), "got: {resp}");
    s.close();
}

#[test]
fn pipelined_requests_keep_order() {
    let mut s = StdioSession::spawn();
    // Pipeline 3 requests, read 3 responses in order.
    writeln!(
        s.stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"version","params":{{}}}}"#
    )
    .unwrap();
    writeln!(
        s.stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"health","params":{{}}}}"#
    )
    .unwrap();
    writeln!(
        s.stdin,
        r#"{{"jsonrpc":"2.0","id":3,"method":"version","params":{{}}}}"#
    )
    .unwrap();
    s.stdin.flush().unwrap();

    let mut r1 = String::new();
    s.stdout.read_line(&mut r1).unwrap();
    let mut r2 = String::new();
    s.stdout.read_line(&mut r2).unwrap();
    let mut r3 = String::new();
    s.stdout.read_line(&mut r3).unwrap();

    assert!(r1.contains("\"id\":1"), "got: {r1}");
    assert!(r2.contains("\"id\":2"), "got: {r2}");
    assert!(r3.contains("\"id\":3"), "got: {r3}");
    s.close();
}

#[test]
fn close_method_exits_cleanly() {
    let mut s = StdioSession::spawn();
    let resp = s.send(r#"{"jsonrpc":"2.0","id":1,"method":"close","params":{}}"#);
    assert!(resp.contains("\"id\":1"), "got: {resp}");
    // Wait for the child to exit.
    let status = s.child.wait().expect("wait child");
    assert!(status.success(), "exit status: {status:?}");
}

#[test]
fn insert_then_query_round_trip() {
    let mut s = StdioSession::spawn();
    let r1 = s.send(
        r#"{"jsonrpc":"2.0","id":1,"method":"insert","params":{"collection":"users","payload":{"name":"Alice","age":30}}}"#,
    );
    assert!(r1.contains("\"affected\":1"), "got: {r1}");

    let r2 = s.send(
        r#"{"jsonrpc":"2.0","id":2,"method":"query","params":{"sql":"SELECT * FROM users"}}"#,
    );
    assert!(r2.contains("\"result\""), "got: {r2}");
    assert!(r2.contains("Alice"), "got: {r2}");
    s.close();
}
