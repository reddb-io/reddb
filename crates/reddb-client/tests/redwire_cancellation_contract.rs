#![cfg(all(feature = "embedded", feature = "redwire"))]

use reddb_client::{ErrorCode, Reddb, ValueOut};
use reddb_server::{RedDBOptions, RedDBRuntime};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_reply_cannot_be_returned_for_the_next_query() {
    tokio::time::timeout(Duration::from_secs(15), cancellation_scenario(false))
        .await
        .expect("bounded cancellation regression");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_committed_write_is_not_replayed_or_misattributed() {
    tokio::time::timeout(Duration::from_secs(15), cancellation_scenario(true))
        .await
        .expect("bounded write cancellation regression");
}

async fn cancellation_scenario(write: bool) {
    let runtime = Arc::new(
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_auth(
            reddb_server::auth::AuthConfig {
                enabled: false,
                ..Default::default()
            },
        ))
        .expect("disposable runtime"),
    );
    runtime
        .execute_query("CREATE TABLE cancellation (id INT, payload TEXT)")
        .expect("schema");
    runtime
        .execute_query("INSERT INTO cancellation (id, payload) VALUES (7, 'old reply')")
        .expect("seed");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("isolated listener");
    let server_address = listener.local_addr().expect("server address");
    let server_runtime = Arc::clone(&runtime);
    let server = tokio::spawn(async move {
        reddb_server::wire::redwire::start_redwire_listener_on(listener, server_runtime)
            .await
            .expect("serve");
    });
    let proxy_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("proxy listener");
    let proxy_address = proxy_listener.local_addr().expect("proxy address");
    let armed = Arc::new(AtomicBool::new(false));
    let received = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (proxy_armed, proxy_received, proxy_release) =
        (armed.clone(), received.clone(), release.clone());
    let proxy = tokio::spawn(async move {
        let (client, _) = proxy_listener.accept().await.expect("accept client");
        let upstream = TcpStream::connect(server_address)
            .await
            .expect("connect upstream");
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = upstream.into_split();
        let responses = async {
            let mut bytes = vec![0; 65536];
            loop {
                let length = server_read.read(&mut bytes).await?;
                if length == 0 {
                    break;
                }
                if proxy_armed.swap(false, Ordering::SeqCst) {
                    proxy_received.notify_one();
                    proxy_release.notified().await;
                }
                client_write.write_all(&bytes[..length]).await?;
            }
            std::io::Result::Ok(())
        };
        tokio::select! {
            _ = tokio::io::copy(&mut client_read, &mut server_write) => {},
            _ = responses => {},
        }
    });
    let client = Arc::new(
        Reddb::connect(&format!("red://{proxy_address}"))
            .await
            .expect("connect"),
    );
    armed.store(true, Ordering::SeqCst);
    let first_client = Arc::clone(&client);
    let first = tokio::spawn(async move {
        first_client
            .query(if write {
                "INSERT INTO cancellation (id, payload) VALUES (8, 'committed once')"
            } else {
                "SELECT payload FROM cancellation"
            })
            .await
    });
    received.notified().await;
    first.abort();
    assert!(first
        .await
        .expect_err("cancelled query task")
        .is_cancelled());
    release.notify_one();
    let next = client.query("SELECT id FROM cancellation").await;
    if write {
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM cancellation WHERE id = 8")
                .expect("inspect committed write")
                .result
                .records
                .len(),
            1
        );
    }
    proxy.abort();
    server.abort();
    match next {
        Ok(result) => {
            assert_eq!(
                result.columns,
                ["id"],
                "cancelled reply was assigned to a different query"
            );
            if !write {
                assert_eq!(result.rows, [vec![("id".to_owned(), ValueOut::Integer(7))]]);
            } else {
                assert_eq!(result.rows.len(), 2);
            }
        }
        Err(error) => assert!(
            matches!(error.code, ErrorCode::Network | ErrorCode::Protocol),
            "unsafe stream must be rejected explicitly: {error}"
        ),
    }
}
