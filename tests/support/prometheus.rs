use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use prost::Message;
use reddb::server::RedDBServer;
use reddb::RedDBRuntime;

#[derive(Clone, PartialEq, Message)]
pub struct WriteRequest {
    #[prost(message, repeated, tag = "1")]
    pub timeseries: Vec<TimeSeries>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TimeSeries {
    #[prost(message, repeated, tag = "1")]
    pub labels: Vec<Label>,
    #[prost(message, repeated, tag = "2")]
    pub samples: Vec<Sample>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Label {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Sample {
    #[prost(double, tag = "1")]
    pub value: f64,
    #[prost(int64, tag = "2")]
    pub timestamp: i64,
}

pub fn label(name: &str, value: &str) -> Label {
    Label {
        name: name.to_string(),
        value: value.to_string(),
    }
}

pub fn sample(value: f64, timestamp: i64) -> Sample {
    Sample { value, timestamp }
}

pub fn post_remote_write(
    rt: RedDBRuntime,
    collection: &str,
    request: &WriteRequest,
) -> (u16, String) {
    let mut protobuf = Vec::new();
    request
        .encode(&mut protobuf)
        .expect("remote_write protobuf should encode");
    let body = snap::raw::Encoder::new()
        .compress_vec(&protobuf)
        .expect("remote_write body should snappy-compress");
    with_one_request_server(rt, |addr| {
        let mut request = format!(
            "POST /api/v1/write?collection={collection} HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Encoding: snappy\r\n\
             Content-Type: application/x-protobuf\r\n\
             X-Prometheus-Remote-Write-Version: 0.1.0\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(&body);
        http_request(addr, request)
    })
}

pub fn get(rt: RedDBRuntime, path: &str) -> (u16, String) {
    with_one_request_server(rt, |addr| {
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .into_bytes();
        http_request(addr, request)
    })
}

pub fn encode_query_value(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn with_one_request_server(
    rt: RedDBRuntime,
    send: impl FnOnce(&str) -> (u16, String),
) -> (u16, String) {
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let handle = thread::spawn(move || server.serve_one_on(listener));
    let response = send(&addr.to_string());
    handle
        .join()
        .expect("server thread should join")
        .expect("server should serve one request");
    response
}

fn http_request(addr: &str, request: Vec<u8>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");
    stream.write_all(&request).expect("write request");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    let response = String::from_utf8_lossy(&response).into_owned();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}
