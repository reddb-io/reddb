use std::net::SocketAddr;
use std::sync::Arc;

use reddb::api::RedDBOptions;
use reddb::wire::postgres::{start_pg_wire_listener, PgOid, PgWireConfig};
use reddb::RedDBRuntime;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn postgres_extended_query_binds_and_returns_rows() {
    let (addr, _server) = start_server().await;
    let mut stream = TcpStream::connect(addr).await.expect("connect pg wire");

    write_startup(&mut stream).await;
    read_until_ready(&mut stream).await;

    write_frontend_frame(
        &mut stream,
        b'P',
        parse_body("", "SELECT $1::int", &[PgOid::Int4.as_u32()]),
    )
    .await;
    write_frontend_frame(
        &mut stream,
        b'B',
        bind_body("", "", &[0], &[Some(b"42".as_slice())], &[]),
    )
    .await;
    write_frontend_frame(&mut stream, b'D', describe_body(b'P', "")).await;
    write_frontend_frame(&mut stream, b'E', execute_body("", 0)).await;
    write_frontend_frame(&mut stream, b'S', Vec::new()).await;

    let frames = read_until_ready(&mut stream).await;
    assert_eq!(
        frames.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
        b"12TDCZ"
    );
    let cells = decode_data_row(&frames[3].1);
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].as_deref(), Some(b"42".as_slice()));
    assert_eq!(decode_command_complete(&frames[4].1), "SELECT 1");

    write_frontend_frame(&mut stream, b'X', Vec::new()).await;
}

#[tokio::test]
async fn postgres_extended_execute_emits_row_description_without_describe() {
    let (addr, _server) = start_server().await;
    let mut stream = TcpStream::connect(addr).await.expect("connect pg wire");

    write_startup(&mut stream).await;
    read_until_ready(&mut stream).await;

    write_frontend_frame(
        &mut stream,
        b'P',
        parse_body("", "SELECT $1::int", &[PgOid::Int4.as_u32()]),
    )
    .await;
    write_frontend_frame(
        &mut stream,
        b'B',
        bind_body("", "", &[0], &[Some(b"42".as_slice())], &[]),
    )
    .await;
    write_frontend_frame(&mut stream, b'E', execute_body("", 0)).await;
    write_frontend_frame(&mut stream, b'S', Vec::new()).await;

    let frames = read_until_ready(&mut stream).await;
    assert_eq!(
        frames.iter().map(|(tag, _)| *tag).collect::<Vec<_>>(),
        b"12TDCZ"
    );
    let cells = decode_data_row(&frames[3].1);
    assert_eq!(cells[0].as_deref(), Some(b"42".as_slice()));

    write_frontend_frame(&mut stream, b'X', Vec::new()).await;
}

#[tokio::test]
async fn postgres_extended_query_supports_binary_text_params_insert_and_close() {
    let (addr, _server) = start_server().await;
    let mut stream = TcpStream::connect(addr).await.expect("connect pg wire");

    write_startup(&mut stream).await;
    read_until_ready(&mut stream).await;

    write_frontend_frame(
        &mut stream,
        b'Q',
        query_body("CREATE TABLE pg_ext_items (id INT, name TEXT)"),
    )
    .await;
    let create_frames = read_until_ready(&mut stream).await;
    assert_eq!(create_frames.last().map(|(tag, _)| *tag), Some(b'Z'));

    write_frontend_frame(
        &mut stream,
        b'P',
        parse_body(
            "ins",
            "INSERT INTO pg_ext_items (id, name) VALUES ($1, $2)",
            &[PgOid::Int8.as_u32(), PgOid::Text.as_u32()],
        ),
    )
    .await;
    write_frontend_frame(
        &mut stream,
        b'B',
        bind_body(
            "ins_portal",
            "ins",
            &[1, 0],
            &[Some(&7i64.to_be_bytes()), Some(b"alice".as_slice())],
            &[],
        ),
    )
    .await;
    write_frontend_frame(&mut stream, b'E', execute_body("ins_portal", 0)).await;
    write_frontend_frame(&mut stream, b'C', close_body(b'P', "ins_portal")).await;
    write_frontend_frame(&mut stream, b'C', close_body(b'S', "ins")).await;
    write_frontend_frame(&mut stream, b'S', Vec::new()).await;

    let insert_frames = read_until_ready(&mut stream).await;
    assert_eq!(
        insert_frames
            .iter()
            .map(|(tag, _)| *tag)
            .collect::<Vec<_>>(),
        b"12C33Z"
    );
    assert_eq!(decode_command_complete(&insert_frames[2].1), "INSERT 0 1");

    write_frontend_frame(
        &mut stream,
        b'P',
        parse_body(
            "sel",
            "SELECT id, name FROM pg_ext_items WHERE id = $1 AND name = $2",
            &[PgOid::Int4.as_u32(), PgOid::Text.as_u32()],
        ),
    )
    .await;
    write_frontend_frame(
        &mut stream,
        b'B',
        bind_body(
            "sel_portal",
            "sel",
            &[0],
            &[Some(b"7".as_slice()), Some(b"alice".as_slice())],
            &[],
        ),
    )
    .await;
    write_frontend_frame(&mut stream, b'D', describe_body(b'P', "sel_portal")).await;
    write_frontend_frame(&mut stream, b'E', execute_body("sel_portal", 0)).await;
    write_frontend_frame(&mut stream, b'C', close_body(b'P', "sel_portal")).await;
    write_frontend_frame(&mut stream, b'C', close_body(b'S', "sel")).await;
    write_frontend_frame(&mut stream, b'S', Vec::new()).await;

    let select_frames = read_until_ready(&mut stream).await;
    assert_eq!(
        select_frames
            .iter()
            .map(|(tag, _)| *tag)
            .collect::<Vec<_>>(),
        b"12TDC33Z"
    );
    let cells = decode_data_row(&select_frames[3].1);
    assert_eq!(cells.len(), 2);
    assert_eq!(cells[0].as_deref(), Some(b"7".as_slice()));
    assert_eq!(cells[1].as_deref(), Some(b"alice".as_slice()));
    assert_eq!(decode_command_complete(&select_frames[4].1), "SELECT 1");

    write_frontend_frame(&mut stream, b'X', Vec::new()).await;
}

async fn start_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let cfg = PgWireConfig {
        bind_addr: addr.to_string(),
        ..PgWireConfig::default()
    };
    let handle = tokio::spawn(async move {
        let _ = start_pg_wire_listener(cfg, runtime).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (addr, handle)
}

async fn write_startup<W: AsyncWrite + Unpin>(stream: &mut W) {
    let mut payload = Vec::new();
    payload.extend_from_slice(&((3u32) << 16).to_be_bytes());
    payload.extend_from_slice(b"user\0reddb\0");
    payload.push(0);
    let len = (payload.len() + 4) as u32;
    stream.write_all(&len.to_be_bytes()).await.unwrap();
    stream.write_all(&payload).await.unwrap();
}

async fn write_frontend_frame<W: AsyncWrite + Unpin>(stream: &mut W, tag: u8, payload: Vec<u8>) {
    stream.write_all(&[tag]).await.unwrap();
    stream
        .write_all(&((payload.len() + 4) as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(&payload).await.unwrap();
}

async fn read_backend_frame<R: AsyncRead + Unpin>(stream: &mut R) -> (u8, Vec<u8>) {
    let mut tag = [0u8; 1];
    stream.read_exact(&mut tag).await.unwrap();
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await.unwrap();
    let len = u32::from_be_bytes(len) as usize;
    let mut body = vec![0u8; len - 4];
    stream.read_exact(&mut body).await.unwrap();
    (tag[0], body)
}

async fn read_until_ready<R: AsyncRead + Unpin>(stream: &mut R) -> Vec<(u8, Vec<u8>)> {
    let mut frames = Vec::new();
    loop {
        let frame = read_backend_frame(stream).await;
        let done = frame.0 == b'Z';
        frames.push(frame);
        if done {
            return frames;
        }
    }
}

fn query_body(query: &str) -> Vec<u8> {
    let mut out = Vec::new();
    push_pg_cstring(&mut out, query);
    out
}

fn parse_body(statement: &str, query: &str, oids: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    push_pg_cstring(&mut out, statement);
    push_pg_cstring(&mut out, query);
    out.extend_from_slice(&(oids.len() as i16).to_be_bytes());
    for oid in oids {
        out.extend_from_slice(&oid.to_be_bytes());
    }
    out
}

fn bind_body(
    portal: &str,
    statement: &str,
    formats: &[i16],
    params: &[Option<&[u8]>],
    result_formats: &[i16],
) -> Vec<u8> {
    let mut out = Vec::new();
    push_pg_cstring(&mut out, portal);
    push_pg_cstring(&mut out, statement);
    out.extend_from_slice(&(formats.len() as i16).to_be_bytes());
    for format in formats {
        out.extend_from_slice(&format.to_be_bytes());
    }
    out.extend_from_slice(&(params.len() as i16).to_be_bytes());
    for param in params {
        match param {
            Some(bytes) => {
                out.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                out.extend_from_slice(bytes);
            }
            None => out.extend_from_slice(&(-1i32).to_be_bytes()),
        }
    }
    out.extend_from_slice(&(result_formats.len() as i16).to_be_bytes());
    for format in result_formats {
        out.extend_from_slice(&format.to_be_bytes());
    }
    out
}

fn describe_body(target: u8, name: &str) -> Vec<u8> {
    let mut out = vec![target];
    push_pg_cstring(&mut out, name);
    out
}

fn execute_body(portal: &str, max_rows: u32) -> Vec<u8> {
    let mut out = Vec::new();
    push_pg_cstring(&mut out, portal);
    out.extend_from_slice(&max_rows.to_be_bytes());
    out
}

fn close_body(target: u8, name: &str) -> Vec<u8> {
    let mut out = vec![target];
    push_pg_cstring(&mut out, name);
    out
}

fn decode_data_row(body: &[u8]) -> Vec<Option<Vec<u8>>> {
    let count = i16::from_be_bytes([body[0], body[1]]) as usize;
    let mut pos = 2;
    let mut cells = Vec::with_capacity(count);
    for _ in 0..count {
        let len = i32::from_be_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
        pos += 4;
        if len < 0 {
            cells.push(None);
        } else {
            let len = len as usize;
            cells.push(Some(body[pos..pos + len].to_vec()));
            pos += len;
        }
    }
    cells
}

fn decode_command_complete(body: &[u8]) -> &str {
    let nul = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    std::str::from_utf8(&body[..nul]).unwrap()
}

fn push_pg_cstring(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(value.as_bytes());
    out.push(0);
}
