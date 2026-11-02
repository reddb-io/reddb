use super::*;
use crate::serde_json::Value as JsonValue;
use crate::storage::memory_pools::MemoryPool;
use reddb_wire::redwire::{decode_frame, Flags, MessageKind, FRAME_HEADER_SIZE};
use std::future::Future;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::time::timeout;

fn runtime() -> Arc<RedDBRuntime> {
    Arc::new(
        RedDBRuntime::with_options(
            crate::RedDBOptions::in_memory().with_memory_budget(1024 * 1024),
        )
        .expect("bounded runtime"),
    )
}

fn charged(runtime: &RedDBRuntime) -> u64 {
    runtime
        .memory_accounting()
        .used_bytes(MemoryPool::QueryBuffers)
}

fn reply(size: usize) -> Frame {
    reddb_wire::redwire::build_reply_frame(17, MessageKind::Result, vec![b'x'; size])
        .expect("reply fixture")
}

fn binary_response(
    runtime: &RedDBRuntime,
    size: usize,
) -> crate::wire::listener::LegacyQueryResponse {
    let result = crate::runtime::RuntimeQueryResult::ok_records(
        "select".into(),
        vec!["payload".into()],
        vec![vec![(
            "payload".into(),
            reddb_types::Value::text("x".repeat(size)),
        )]],
        "select",
    );
    crate::wire::listener::LegacyQueryResponse::Admitted(
        runtime.serialize_binary_result(&result).unwrap(),
    )
}

#[tokio::test]
async fn binary_response_admission_survives_framing_queue_and_clones_without_double_charge() {
    let runtime = runtime();
    let response = binary_response(&runtime, 64 * 1024);
    let expected = reddb_wire::redwire::rewrap_length_prefixed_handler_response(&response, 91);
    let original_length = response.len();
    let encoded_length = original_length - 5 + FRAME_HEADER_SIZE;
    let retained = match &response {
        crate::wire::listener::LegacyQueryResponse::Admitted(bytes) => bytes.slice(..1),
        _ => panic!("materialized result is admitted"),
    };
    runtime.refresh_memory_accounting();
    let headroom =
        runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
    let filler = runtime
        .serialize_query_buffer(headroom as usize - encoded_length - 8, 0, |_| {})
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let sender = FrameTx::new(tx, Arc::clone(&runtime));
    sender
        .send_legacy_response(response, 91)
        .await
        .expect("only one output copy needs new admission");
    drop(filler);
    assert_eq!(charged(&runtime), (original_length + encoded_length) as u64);
    drop(retained);
    assert_eq!(charged(&runtime), encoded_length as u64);
    let bytes = rx.recv().await.unwrap();
    assert_eq!(decode_frame(&bytes).unwrap().0, expected);
    let slice = bytes.slice(..1);
    drop(bytes);
    assert_eq!(charged(&runtime), encoded_length as u64);
    drop(slice);
    assert_eq!(charged(&runtime), 0);
}

#[tokio::test]
async fn binary_response_blocked_send_and_failed_framing_release_owned_reservations() {
    let runtime = runtime();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let sender = FrameTx::new(tx, Arc::clone(&runtime));
    sender
        .send_legacy_response(binary_response(&runtime, 4096), 1)
        .await
        .unwrap();
    let queued = charged(&runtime);
    {
        let response = binary_response(&runtime, 8192);
        let encoded = response.len() - 5 + FRAME_HEADER_SIZE;
        let pending = sender.send_legacy_response(response, 2);
        tokio::pin!(pending);
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                pending.as_mut().poll(cx).is_pending()
            ))
            .await
        );
        assert_eq!(charged(&runtime), queued + encoded as u64);
    }
    assert_eq!(charged(&runtime), queued);
    drop(rx.recv().await.unwrap());
    let response = binary_response(&runtime, 64 * 1024);
    runtime.refresh_memory_accounting();
    let headroom =
        runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
    let filler = runtime
        .serialize_query_buffer(headroom as usize - 16 * 1024, 0, |_| {})
        .unwrap();
    let error = sender.send_legacy_response(response, 3).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::OutOfMemory);
    timeout(Duration::from_secs(5), sender.failed())
        .await
        .unwrap();
    drop(filler);
    assert_eq!(charged(&runtime), 0);
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    FrameTx::new(tx, Arc::clone(&runtime))
        .send_legacy_response(binary_response(&runtime, 64 * 1024), 4)
        .await
        .unwrap();
    drop(rx.recv().await.unwrap());
    assert_eq!(charged(&runtime), 0);
}

#[tokio::test]
async fn blocked_output_stream_retains_result_admission_until_cancelled() {
    let runtime = runtime();
    runtime
        .execute_query("CREATE TABLE stream_owners (payload TEXT)")
        .unwrap();
    for _ in 0..16 {
        runtime
            .execute_query_with_params(
                "INSERT INTO stream_owners (payload) VALUES ($1)",
                &[reddb_types::Value::text("x".repeat(8192))],
            )
            .unwrap();
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let sender = FrameTx::new(tx, Arc::clone(&runtime));
    let (_cancel, cancel_rx) = tokio::sync::oneshot::channel();
    {
        let stream = run_output_stream(
            Arc::clone(&runtime),
            17,
            7,
            parse_open_stream(br#"{"sql":"SELECT payload FROM stream_owners"}"#).unwrap(),
            false,
            cancel_rx,
            sender,
            OperationContext::read_only("stream-memory-fixture"),
            Role::Read,
        );
        tokio::pin!(stream);
        assert!(
            std::future::poll_fn(|cx| {
                std::task::Poll::Ready(stream.as_mut().poll(cx).is_pending())
            })
            .await
        );
        let ack = rx.recv().await.expect("OpenAck before rows");
        assert_eq!(decode_frame(&ack).unwrap().0.kind, MessageKind::OpenAck);
        drop(ack);
        assert!(charged(&runtime) >= 16 * 8192,
            "the blocked producer must retain admission for all materialized rows, not just its encoded chunk");
        // A disconnected session cancels its worker future. The retained
        // result and not-yet-queued response must both return their budget.
    }
    assert_eq!(charged(&runtime), 0);
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn oversized_stream_row_reports_error_instead_of_empty_success() {
    let runtime = Arc::new(
        RedDBRuntime::with_options(
            crate::RedDBOptions::in_memory().with_memory_budget(128 * 1024 * 1024),
        )
        .unwrap(),
    );
    runtime
        .execute_query("CREATE TABLE stream_oversize (payload TEXT)")
        .unwrap();
    runtime
        .execute_query_with_params(
            "INSERT INTO stream_oversize (payload) VALUES ($1)",
            &[reddb_types::Value::text(
                "x".repeat(reddb_wire::redwire::MAX_FRAME_SIZE as usize),
            )],
        )
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let (_cancel, cancel_rx) = tokio::sync::oneshot::channel();
    run_output_stream(
        Arc::clone(&runtime),
        17,
        7,
        parse_open_stream(br#"{"sql":"SELECT payload FROM stream_oversize"}"#).unwrap(),
        false,
        cancel_rx,
        FrameTx::new(tx, Arc::clone(&runtime)),
        OperationContext::read_only("stream-oversize-fixture"),
        Role::Read,
    )
    .await;
    let ack = rx.recv().await.unwrap();
    assert_eq!(decode_frame(&ack).unwrap().0.kind, MessageKind::OpenAck);
    drop(ack);
    let error = rx.recv().await.unwrap();
    let frame = decode_frame(&error).unwrap().0;
    assert_eq!(
        frame.kind,
        MessageKind::StreamError,
        "an unencodable row must not disappear into a successful StreamEnd"
    );
    let payload = crate::serde_json::from_slice::<JsonValue>(&frame.payload).unwrap();
    assert_eq!(
        payload["code"].as_str(),
        Some("stream_chunk_encoding_failed")
    );
    drop(error);
    let end = rx.recv().await.unwrap();
    assert_eq!(decode_frame(&end).unwrap().0.kind, MessageKind::StreamEnd);
    drop(end);
    assert!(rx.recv().await.is_none());
    assert_eq!(charged(&runtime), 0);
}

#[tokio::test]
async fn cancelling_a_full_queue_send_releases_only_its_own_buffer() {
    let runtime = runtime();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let sender = FrameTx::new(tx, Arc::clone(&runtime));
    sender.send_frame(reply(4096)).await.unwrap();
    let original_charge = charged(&runtime);
    {
        let pending = sender.send_frame(reply(8192));
        tokio::pin!(pending);
        assert!(
            std::future::poll_fn(|cx| {
                std::task::Poll::Ready(pending.as_mut().poll(cx).is_pending())
            })
            .await
        );
        assert_eq!(
            charged(&runtime),
            original_charge + 8192 + FRAME_HEADER_SIZE as u64
        );
        // Dropping this blocked producer must preserve the first queue owner.
    }
    assert_eq!(charged(&runtime), original_charge);
    drop(rx.recv().await.unwrap());
    assert_eq!(charged(&runtime), 0);
    sender
        .send_frame(reply(8192))
        .await
        .expect("retry after cancellation");
    drop(rx);
    assert_eq!(charged(&runtime), 0);
}

#[tokio::test]
async fn socket_blocked_by_slow_reader_retains_charge_then_drains_on_clean_close() {
    let runtime = runtime();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let sender = FrameTx::new(tx, Arc::clone(&runtime));
    let (writer, mut peer) = tokio::io::duplex(1);
    let task = sender.spawn_writer(rx, writer);
    sender.send_frame(reply(128 * 1024)).await.unwrap();
    let mut received = vec![0; 1];
    timeout(Duration::from_secs(5), peer.read_exact(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(charged(&runtime), 128 * 1024 + FRAME_HEADER_SIZE as u64);
    assert!(
        !task.is_finished(),
        "socket cannot write the rest until peer reads"
    );
    drop(sender);
    timeout(Duration::from_secs(5), peer.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    let (decoded, used) = decode_frame(&received).unwrap();
    assert_eq!(used, received.len());
    assert_eq!(decoded.payload, vec![b'x'; 128 * 1024]);
    assert_eq!(decoded.correlation_id, 17);
    assert_eq!(charged(&runtime), 0);
}

#[tokio::test]
async fn admission_failure_unblocks_socket_and_full_queue_and_refuses_later_success() {
    let runtime = runtime();
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let sender = FrameTx::new(tx, Arc::clone(&runtime));
    let (writer, mut peer) = tokio::io::duplex(1);
    let task = sender.spawn_writer(rx, writer);
    sender.send_frame(reply(4096)).await.unwrap();
    timeout(Duration::from_secs(5), peer.read_exact(&mut [0]))
        .await
        .unwrap()
        .unwrap();
    sender.send_frame(reply(8192)).await.unwrap();
    let pending = sender.send_frame(reply(16384));
    tokio::pin!(pending);
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(pending.as_mut().poll(cx).is_pending())
        })
        .await
    );
    let error = sender.send_frame(reply(1024 * 1024)).await.unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::OutOfMemory);
    timeout(Duration::from_secs(5), sender.failed())
        .await
        .unwrap();
    assert!(timeout(Duration::from_secs(5), &mut pending)
        .await
        .unwrap()
        .is_err());
    timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        charged(&runtime),
        0,
        "queued, blocked producer and socket owners released"
    );
    assert!(sender
        .send_frame(
            reddb_wire::redwire::build_reply_frame(17, MessageKind::StreamEnd, vec![])
                .expect("terminal fixture")
        )
        .await
        .is_err());
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    FrameTx::new(tx, Arc::clone(&runtime))
        .send_frame(reply(4096))
        .await
        .expect("new connection can retry");
    drop(rx.recv().await.unwrap());
    assert_eq!(charged(&runtime), 0);
}

#[tokio::test]
async fn socket_error_wakes_session_and_releases_queue() {
    let runtime = runtime();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let sender = FrameTx::new(tx, Arc::clone(&runtime));
    sender.send_frame(reply(4096)).await.unwrap();
    sender.send_frame(reply(8192)).await.unwrap();
    let (writer, peer) = tokio::io::duplex(1);
    drop(peer);
    let task = sender.spawn_writer(rx, writer);
    timeout(Duration::from_secs(5), sender.failed())
        .await
        .unwrap();
    timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(charged(&runtime), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_four_and_sixteen_connections_compete_for_one_budget_and_recover() {
    let runtime = runtime();
    for connections in [1, 4, 16] {
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..connections {
            let runtime = Arc::clone(&runtime);
            tasks.spawn(async move {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                let result = FrameTx::new(tx, runtime)
                    .send_frame(reply(128 * 1024))
                    .await;
                (result, rx)
            });
        }
        let mut queues = Vec::new();
        let mut admitted = 0;
        let mut denied = 0;
        while let Some(result) = tasks.join_next().await {
            let (result, receiver) = result.unwrap();
            match result {
                Ok(()) => admitted += 1,
                Err(error) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::OutOfMemory);
                    denied += 1;
                }
            }
            queues.push(receiver);
        }
        assert!(admitted > 0);
        if connections == 16 {
            assert!(denied > 0, "held responses cannot exceed the shared budget");
        }
        assert_eq!(
            charged(&runtime),
            admitted * (128 * 1024 + FRAME_HEADER_SIZE as u64)
        );
        assert!(
            runtime
                .memory_accounting()
                .enforcement_snapshot()
                .high_water_bytes
                <= runtime.memory_budget().resolved_bytes
        );
        drop(queues);
        assert_eq!(
            charged(&runtime),
            0,
            "all connections return their reservations"
        );
    }
}

#[tokio::test]
async fn compression_request_uses_valid_plaintext_fallback_and_preserves_metadata() {
    let runtime = runtime();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let sender = FrameTx::new(tx, Arc::clone(&runtime));
    let mut frame = reply(4096);
    frame.flags = Flags::COMPRESSED.insert(Flags::MORE_FRAMES);
    frame.stream_id = 7;
    sender.send_frame(frame).await.unwrap();
    let bytes = rx.recv().await.unwrap();
    let (decoded, used) = decode_frame(&bytes).unwrap();
    assert_eq!(used, bytes.len());
    assert_eq!(decoded.flags, Flags::MORE_FRAMES);
    assert_eq!(decoded.stream_id, 7);
    assert_eq!(decoded.correlation_id, 17);
    assert_eq!(decoded.payload, vec![b'x'; 4096]);
    drop(bytes);
    assert_eq!(charged(&runtime), 0);
}
