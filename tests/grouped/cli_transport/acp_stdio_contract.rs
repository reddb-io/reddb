use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use agent_client_protocol_schema::{v1, v2};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

struct AcpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    output: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl AcpProcess {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_red"))
            .arg("acp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn red acp");
        let stdin = child.stdin.take().expect("capture ACP stdin");
        let stdout = child.stdout.take().expect("capture ACP stdout");
        let (sender, output) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        Self {
            child,
            stdin: Some(stdin),
            output,
            reader: Some(reader),
        }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().expect("ACP stdin is open");
        serde_json::to_writer(&mut *stdin, &message).expect("serialize ACP request");
        stdin.write_all(b"\n").expect("frame ACP request");
        stdin.flush().expect("flush ACP request");
    }

    fn receive(&self) -> Value {
        let line = self
            .output
            .recv_timeout(RESPONSE_TIMEOUT)
            .expect("timed out waiting for ACP output");
        serde_json::from_str(&line).expect("ACP output is JSON")
    }
}

impl Drop for AcpProcess {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

fn result<T: DeserializeOwned>(message: &Value) -> T {
    serde_json::from_value(message["result"].clone()).unwrap_or_else(|error| {
        panic!("response does not match the official ACP schema: {error}; response={message}")
    })
}

fn cwd() -> String {
    std::env::current_dir()
        .expect("current directory")
        .display()
        .to_string()
}

#[test]
fn red_acp_v1_process_matches_the_official_schema() {
    let mut acp = AcpProcess::spawn();
    acp.send(request(1, "initialize", json!({ "protocolVersion": 1 })));
    let initialized = acp.receive();
    let _: v1::InitializeResponse = result(&initialized);

    acp.send(request(
        2,
        "session/new",
        json!({ "cwd": cwd(), "mcpServers": [] }),
    ));
    let created = acp.receive();
    let _: v1::NewSessionResponse = result(&created);
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("v1 session id")
        .to_string();

    acp.send(request(
        3,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": "???" }],
        }),
    ));
    let prompt_error = acp.receive();
    assert_eq!(prompt_error["id"], 3);
    assert_eq!(prompt_error["error"]["code"], -32603);

    acp.send(request(
        4,
        "session/close",
        json!({ "sessionId": session_id }),
    ));
    let _: v1::CloseSessionResponse = result(&acp.receive());
}

#[test]
fn red_acp_v2_process_matches_the_official_schema_and_lifecycle() {
    let mut acp = AcpProcess::spawn();
    acp.send(request(
        1,
        "initialize",
        json!({
            "protocolVersion": 2,
            "info": { "name": "reddb-contract-test", "version": "1.0.0" },
        }),
    ));
    let _: v2::InitializeResponse = result(&acp.receive());

    let working_directory = cwd();
    acp.send(request(
        2,
        "session/new",
        json!({ "cwd": working_directory }),
    ));
    let created = acp.receive();
    let _: v2::NewSessionResponse = result(&created);
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("v2 session id")
        .to_string();

    acp.send(request(3, "session/list", json!({})));
    let listed = acp.receive();
    let _: v2::ListSessionsResponse = result(&listed);
    assert_eq!(listed["result"]["sessions"][0]["sessionId"], session_id);

    acp.send(request(
        4,
        "session/resume",
        json!({ "cwd": working_directory, "sessionId": session_id }),
    ));
    let resumed_state = acp.receive();
    assert_eq!(resumed_state["method"], "session/update");
    let _: v2::UpdateSessionNotification = serde_json::from_value(resumed_state["params"].clone())
        .expect("resume update matches the official ACP v2 schema");
    let _: v2::ResumeSessionResponse = result(&acp.receive());

    acp.send(request(
        5,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": "???" }],
        }),
    ));
    let _: v2::PromptResponse = result(&acp.receive());

    let mut updates = Vec::new();
    for _ in 0..8 {
        let notification = acp.receive();
        assert_eq!(notification["method"], "session/update");
        let _: v2::UpdateSessionNotification =
            serde_json::from_value(notification["params"].clone())
                .expect("prompt update matches the official ACP v2 schema");
        let update = &notification["params"]["update"];
        updates.push(
            update["sessionUpdate"]
                .as_str()
                .expect("session update discriminator")
                .to_string(),
        );
        if update["sessionUpdate"] == "state_update" && update["state"] == "idle" {
            assert_eq!(update["stopReason"], "error");
            assert!(update["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("no usable tokens")));
            break;
        }
    }
    assert_eq!(updates, ["state_update", "user_message", "state_update"]);

    acp.send(request(
        6,
        "session/close",
        json!({ "sessionId": session_id }),
    ));
    let _: v2::CloseSessionResponse = result(&acp.receive());
}
