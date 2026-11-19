use super::*;

/// Read a full RedWire frame off the client side of the duplex.
async fn read_one_frame<R: tokio::io::AsyncRead + Unpin + Send>(r: &mut R) -> Frame {
    read_frame_async(r).await.expect("read frame")
}

async fn authenticate_anonymous(client: &mut tokio::io::DuplexStream) {
    client.write_all(&[1u8]).await.expect("write minor");
    client
        .write_all(&encode_frame(&hello_frame(1, Some("test"))))
        .await
        .expect("write hello");
    assert_eq!(read_one_frame(client).await.kind, MessageKind::HelloAck);
    client
        .write_all(&encode_frame(&anonymous_auth_response_frame(2)))
        .await
        .expect("write auth response");
    assert_eq!(read_one_frame(client).await.kind, MessageKind::AuthOk);
}

/// The deterministic blocking gate is process-global; tests that arm it must
/// not interleave or one test's install clobbers the other's correlation id.
static BLOCKING_GATE_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct TestBlockingGate {
    correlation_id: u64,
}

impl TestBlockingGate {
    fn install(correlation_id: u64) -> Self {
        use std::sync::atomic::Ordering;

        REDWIRE_TEST_BLOCKING_GATE_ENTERED.store(0, Ordering::Release);
        REDWIRE_TEST_BLOCKING_GATE_CORRELATION.store(correlation_id, Ordering::Release);
        Self { correlation_id }
    }

    fn release(&mut self) {
        use std::sync::atomic::Ordering;

        let _ = REDWIRE_TEST_BLOCKING_GATE_CORRELATION.compare_exchange(
            self.correlation_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

impl Drop for TestBlockingGate {
    fn drop(&mut self) {
        self.release();
    }
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
        crate::wire::protocol::encode_value(&mut p, &reddb_types::Value::Integer(*id));
        crate::wire::protocol::encode_value(&mut p, &reddb_types::Value::text(name.to_string()));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnected_session_cancels_blocking_query_and_releases_execution_permit() {
    use std::sync::atomic::Ordering;

    const CORRELATION_ID: u64 = u64::MAX - 41;
    let _serial = BLOCKING_GATE_SERIAL.lock().await;

    let runtime = std::sync::Arc::new(RedDBRuntime::in_memory().expect("runtime"));
    runtime.configure_execution_admission(1, 0, 1_000);
    let admission = runtime.execution_admission();
    let server_runtime = std::sync::Arc::clone(&runtime);
    let (server_io, mut client) = tokio::io::duplex(64 * 1024);
    let server_task =
        tokio::spawn(async move { handle_session(server_io, server_runtime, None, None).await });

    authenticate_anonymous(&mut client).await;
    let mut gate = TestBlockingGate::install(CORRELATION_ID);
    let query = reddb_wire::redwire::build_query_frame(CORRELATION_ID, "SELECT 1 AS healthy")
        .expect("build blocking query");
    client
        .write_all(&encode_frame(&query))
        .await
        .expect("write blocking query");

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while REDWIRE_TEST_BLOCKING_GATE_ENTERED.load(Ordering::Acquire) != CORRELATION_ID {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("query entered deterministic blocking gate");
    assert_eq!(
        admission.active(),
        1,
        "blocked query must retain the execution permit before disconnect"
    );
    drop(client);

    let mut server_task = server_task;
    let disconnected =
        tokio::time::timeout(std::time::Duration::from_millis(250), &mut server_task).await;
    gate.release();
    if disconnected.is_err() {
        tokio::time::timeout(std::time::Duration::from_secs(1), server_task)
            .await
            .expect("released test gate must let the session finish")
            .expect("session task")
            .expect("clean disconnect after test cleanup");
        panic!("disconnected session left the blocking query and its permit running");
    }
    disconnected
        .expect("checked above")
        .expect("session task")
        .expect("clean disconnect");
    assert_eq!(
        admission.active(),
        0,
        "disconnect must return the shared execution permit"
    );
}

/// Staging incident 2026-11-19 (engine v1.39.6): consumers long-poll with
/// `QUEUE READ … WAIT 30000ms` while `execution_queue_timeout` is 30 s. On an
/// empty queue the statement parks for its full WAIT by design, so the
/// execution watchdog declared the engine wedged and `/health/ready` flapped
/// to 503 every cycle until liveness restarted the pod. Time spent parked in
/// the queue wait must not count toward the deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_read_wait_longer_than_execution_deadline_keeps_readiness() {
    const DEADLINE_MS: u64 = 100;
    const WAIT_MS: u64 = 600;

    let runtime = std::sync::Arc::new(RedDBRuntime::in_memory().expect("runtime"));
    runtime.configure_execution_admission(4, 4, DEADLINE_MS);
    runtime
        .execute_query("CREATE QUEUE idle_jobs")
        .expect("create queue");
    let server_runtime = std::sync::Arc::clone(&runtime);
    let (server_io, mut client) = tokio::io::duplex(64 * 1024);
    let server_task =
        tokio::spawn(async move { handle_session(server_io, server_runtime, None, None).await });
    authenticate_anonymous(&mut client).await;

    let query = reddb_wire::redwire::build_query_frame(
        10,
        &format!("QUEUE READ idle_jobs CONSUMER w1 WAIT {WAIT_MS}ms"),
    )
    .expect("build queue read");
    let started = std::time::Instant::now();
    client
        .write_all(&encode_frame(&query))
        .await
        .expect("write queue read");

    let response = {
        let read = read_one_frame(&mut client);
        tokio::pin!(read);
        loop {
            tokio::select! {
                frame = &mut read => break frame,
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                    assert!(
                        runtime.execution_watchdog().is_ready(),
                        "parked QUEUE READ WAIT degraded readiness after {:?}",
                        started.elapsed()
                    );
                }
            }
        }
    };
    assert_eq!(
        response.kind,
        MessageKind::Result,
        "empty WAIT times out cleanly"
    );
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(WAIT_MS),
        "the read must have parked past the {DEADLINE_MS}ms execution deadline"
    );
    assert!(runtime.execution_watchdog().is_ready());

    drop(client);
    let _ = server_task.await;
}

/// The exemption is scoped to the park: an execution genuinely stuck in
/// engine work still degrades readiness once it overruns the deadline, and
/// recovers only when that worker finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stuck_redwire_execution_still_degrades_readiness_after_deadline() {
    use std::sync::atomic::Ordering;

    const CORRELATION_ID: u64 = u64::MAX - 42;
    let _serial = BLOCKING_GATE_SERIAL.lock().await;

    let runtime = std::sync::Arc::new(RedDBRuntime::in_memory().expect("runtime"));
    runtime.configure_execution_admission(4, 4, 100);
    let server_runtime = std::sync::Arc::clone(&runtime);
    let (server_io, mut client) = tokio::io::duplex(64 * 1024);
    let server_task =
        tokio::spawn(async move { handle_session(server_io, server_runtime, None, None).await });
    authenticate_anonymous(&mut client).await;

    let mut gate = TestBlockingGate::install(CORRELATION_ID);
    let query = reddb_wire::redwire::build_query_frame(CORRELATION_ID, "SELECT 1 AS stuck")
        .expect("build blocking query");
    client
        .write_all(&encode_frame(&query))
        .await
        .expect("write blocking query");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while REDWIRE_TEST_BLOCKING_GATE_ENTERED.load(Ordering::Acquire) != CORRELATION_ID {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("query entered deterministic blocking gate");

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while runtime.execution_watchdog().is_ready() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a wedged execution must degrade readiness after the deadline");

    gate.release();
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        read_one_frame(&mut client),
    )
    .await
    .expect("released query replies");
    assert_eq!(response.kind, MessageKind::Result);
    assert!(
        runtime.execution_watchdog().is_ready(),
        "readiness recovers once the stuck worker completes"
    );

    drop(client);
    let _ = server_task.await;
}

/// Incident follow-up: the async live wait (`QueueWaitOpen` path) must time
/// out on its own schedule while a ticketed SQL `QUEUE READ … WAIT` on the
/// same queue is parked past the execution deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_queue_wait_times_out_on_schedule_beside_a_ticketed_sql_wait() {
    const LIVE_WAIT_MS: u64 = 300;

    let runtime = std::sync::Arc::new(RedDBRuntime::in_memory().expect("runtime"));
    runtime.configure_execution_admission(4, 4, 100);
    runtime
        .execute_query("CREATE QUEUE shared_jobs")
        .expect("create queue");
    let server_runtime = std::sync::Arc::clone(&runtime);
    let (server_io, mut client) = tokio::io::duplex(64 * 1024);
    let server_task =
        tokio::spawn(async move { handle_session(server_io, server_runtime, None, None).await });
    authenticate_anonymous(&mut client).await;
    let sql_wait = reddb_wire::redwire::build_query_frame(
        11,
        "QUEUE READ shared_jobs CONSUMER sql_worker WAIT 1500ms",
    )
    .expect("build queue read");
    client
        .write_all(&encode_frame(&sql_wait))
        .await
        .expect("write queue read");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let started = std::time::Instant::now();
    let outcome = runtime
        .redwire_queue_wait_json(
            "shared_jobs",
            None,
            "live_worker",
            1,
            LIVE_WAIT_MS,
            None,
            None,
            None,
        )
        .await
        .expect("live wait");
    let elapsed = started.elapsed();
    assert!(matches!(
        outcome,
        crate::runtime::RedwireWaitOutcome::TimedOut
    ));
    assert!(
        elapsed >= std::time::Duration::from_millis(LIVE_WAIT_MS)
            && elapsed < std::time::Duration::from_millis(LIVE_WAIT_MS + 400),
        "live wait resolved late: {elapsed:?}"
    );

    let response = read_one_frame(&mut client).await;
    assert_eq!(response.kind, MessageKind::Result);
    assert!(runtime.execution_watchdog().is_ready());
    drop(client);
    let _ = server_task.await;
}
