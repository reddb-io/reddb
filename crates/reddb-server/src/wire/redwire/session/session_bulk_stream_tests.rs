use super::*;

/// Read a full RedWire frame off the client side of the duplex.
async fn read_one_frame<R: tokio::io::AsyncRead + Unpin + Send>(r: &mut R) -> Frame {
    read_frame_async(r).await.expect("read frame")
}

fn request_frame(correlation_id: u64, kind: MessageKind, payload: Vec<u8>) -> Frame {
    reddb_wire::redwire::build_request_frame(correlation_id, kind, payload)
        .expect("build request frame")
}

fn hello_frame(correlation_id: u64, client_name: Option<&str>) -> Frame {
    reddb_wire::redwire::build_client_hello_frame(correlation_id, ["anonymous"], 0, client_name)
        .expect("build hello frame")
}

fn anonymous_auth_response_frame(correlation_id: u64) -> Frame {
    reddb_wire::redwire::build_auth_response_frame(correlation_id, b"{}".to_vec())
        .expect("build auth response frame")
}

fn stream_start_payload(coll: &str, cols: &[&str]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&(coll.len() as u16).to_le_bytes());
    p.extend_from_slice(coll.as_bytes());
    p.extend_from_slice(&(cols.len() as u16).to_le_bytes());
    for c in cols {
        p.extend_from_slice(&(c.len() as u16).to_le_bytes());
        p.extend_from_slice(c.as_bytes());
    }
    p
}

fn stream_rows_payload(rows: &[(i64, &str)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    for (id, name) in rows {
        crate::wire::protocol::encode_value(&mut p, &crate::storage::schema::Value::Integer(*id));
        crate::wire::protocol::encode_value(
            &mut p,
            &crate::storage::schema::Value::text(name.to_string()),
        );
    }
    p
}

/// Regression for #75: BulkStreamRows success must NOT emit a
/// response frame. The legacy handler signals "no response" with
/// an empty Vec; rewrapping that as a RedWire frame sent a stale
/// ack back, and the next BulkStreamCommit response then carried
/// the wrong correlation id (off-by-one) — clients failed with
/// `wire: response correlation mismatch: sent N, got N-1`.
#[tokio::test]
async fn bulk_stream_rows_success_emits_no_response_frame() {
    // Server runtime + table the stream will land into.
    let runtime = std::sync::Arc::new(RedDBRuntime::in_memory().expect("runtime"));
    runtime
        .execute_query("CREATE TABLE target (id INT, name TEXT)")
        .expect("create table");

    // In-memory pipe — server side fed into handle_session, client
    // side speaks raw RedWire.
    let (server_io, mut client) = tokio::io::duplex(64 * 1024);

    let server_task = tokio::spawn(async move {
        let _ = handle_session(server_io, runtime, None, None).await;
    });

    // 1) Send the v2 minor-version byte the listener detector
    //    would have stripped before dispatching to handle_session.
    client.write_all(&[1u8]).await.expect("write minor");

    // 2) Hello — anonymous, since the server has no AuthStore.
    let hello = encode_frame(&hello_frame(1, Some("test")));
    client.write_all(&hello).await.expect("write hello");

    // 3) Read HelloAck.
    let ack = read_one_frame(&mut client).await;
    assert_eq!(ack.kind, MessageKind::HelloAck);

    // 4) AuthResponse (anonymous needs no proof body).
    let authresp = encode_frame(&anonymous_auth_response_frame(2));
    client.write_all(&authresp).await.expect("write authresp");

    // 5) Read AuthOk.
    let auth_ok = read_one_frame(&mut client).await;
    assert_eq!(auth_ok.kind, MessageKind::AuthOk);

    // 6) BulkStreamStart — server sends a BulkStreamAck back.
    let start = encode_frame(&request_frame(
        3,
        MessageKind::BulkStreamStart,
        stream_start_payload("target", &["id", "name"]),
    ));
    client.write_all(&start).await.expect("write start");
    let start_ack = read_one_frame(&mut client).await;
    assert_eq!(start_ack.kind, MessageKind::BulkStreamAck);
    assert_eq!(start_ack.correlation_id, 3);

    // 7) BulkStreamRows — success path MUST NOT emit a response.
    let rows = encode_frame(&request_frame(
        4,
        MessageKind::BulkStreamRows,
        stream_rows_payload(&[(1, "a"), (2, "b")]),
    ));
    client.write_all(&rows).await.expect("write rows");

    // 8) BulkStreamCommit — server replies with BulkOk carrying
    //    correlation_id == 5. If the bug were still present, the
    //    next frame on the wire would be a BulkStreamAck with
    //    correlation_id 4 (the rewrapped empty success vec) and
    //    the assertion below would fail.
    let commit = encode_frame(&request_frame(5, MessageKind::BulkStreamCommit, vec![]));
    client.write_all(&commit).await.expect("write commit");

    let next = read_one_frame(&mut client).await;
    assert_eq!(
        next.kind,
        MessageKind::BulkOk,
        "expected BulkOk after commit; got {:?} — BulkStreamRows leaked an ack frame",
        next.kind
    );
    assert_eq!(
        next.correlation_id, 5,
        "commit response must carry the commit's correlation id"
    );

    // 9) Tear the session down cleanly.
    let bye = encode_frame(&request_frame(6, MessageKind::Bye, vec![]));
    client.write_all(&bye).await.expect("write bye");
    let _ = read_one_frame(&mut client).await; // drain Bye echo
    drop(client);
    let _ = server_task.await;
}

/// The error path for BulkStreamRows still has to produce a
/// terminal frame so the client unblocks on the bad batch.
#[tokio::test]
async fn bulk_stream_rows_error_still_emits_error_frame() {
    let runtime = std::sync::Arc::new(RedDBRuntime::in_memory().expect("runtime"));
    let (server_io, mut client) = tokio::io::duplex(64 * 1024);

    let server_task = tokio::spawn(async move {
        let _ = handle_session(server_io, runtime, None, None).await;
    });

    client.write_all(&[1u8]).await.unwrap();
    client
        .write_all(&encode_frame(&hello_frame(1, None)))
        .await
        .unwrap();
    let _ack = read_one_frame(&mut client).await;
    client
        .write_all(&encode_frame(&anonymous_auth_response_frame(2)))
        .await
        .unwrap();
    let _auth_ok = read_one_frame(&mut client).await;

    // Send BulkStreamRows with no prior BulkStreamStart — the
    // legacy handler returns a non-empty Error frame, which the
    // session must forward.
    let rows = encode_frame(&request_frame(
        7,
        MessageKind::BulkStreamRows,
        stream_rows_payload(&[(1, "a")]),
    ));
    client.write_all(&rows).await.unwrap();
    let resp = read_one_frame(&mut client).await;
    assert_eq!(resp.kind, MessageKind::Error);
    assert_eq!(resp.correlation_id, 7);

    drop(client);
    let _ = server_task.await;
}
