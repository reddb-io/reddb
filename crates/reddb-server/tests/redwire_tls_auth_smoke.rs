//! The standalone TLS listener must use the same auth store as HTTP login.
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;

use reddb_server::auth::{AuthConfig, AuthStore, Role};
use reddb_server::wire::redwire::start_redwire_tls_listener_on;
use reddb_server::wire::tls::generate_self_signed_cert;
use reddb_server::wire::WireTlsConfig;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_wire::redwire::{
    build_auth_response_anonymous_payload, build_auth_response_bearer_payload,
    build_auth_response_frame, build_client_hello_frame, read_frame_async, write_frame_async,
    MessageKind, MAX_KNOWN_MINOR_VERSION, REDWIRE_MAGIC,
};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

async fn authenticate(with_store: bool, method: &str, valid_token: bool) -> MessageKind {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        ..Default::default()
    }));
    store
        .create_user("pilot", "synthetic-test-password", Role::Read)
        .unwrap();
    let session = store
        .authenticate("pilot", "synthetic-test-password")
        .unwrap();
    if with_store {
        runtime.set_auth_store(store);
    }

    let directory = tempfile::tempdir().unwrap();
    let (cert, key) = generate_self_signed_cert("localhost").unwrap();
    let cert_path = directory.path().join("cert.pem");
    let key_path = directory.path().join("key.pem");
    std::fs::write(&cert_path, &cert).unwrap();
    std::fs::write(&key_path, key).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        start_redwire_tls_listener_on(
            listener,
            runtime,
            &WireTlsConfig {
                cert_path,
                key_path,
            },
        )
        .await
        .unwrap();
    });
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_pemfile::certs(&mut BufReader::new(cert.as_bytes())) {
        roots.add(certificate.unwrap()).unwrap();
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let mut socket = connector
        .connect(
            "localhost".try_into().unwrap(),
            TcpStream::connect(address).await.unwrap(),
        )
        .await
        .unwrap();
    socket
        .write_all(&[REDWIRE_MAGIC, MAX_KNOWN_MINOR_VERSION])
        .await
        .unwrap();
    write_frame_async(
        &mut socket,
        &build_client_hello_frame(1, [method], 0, Some("tls-auth-regression")).unwrap(),
    )
    .await
    .unwrap();
    let hello = read_frame_async(&mut socket).await.unwrap();
    let result = if hello.kind != MessageKind::HelloAck {
        hello.kind
    } else {
        let payload = if method == "anonymous" {
            build_auth_response_anonymous_payload()
        } else {
            build_auth_response_bearer_payload(if valid_token {
                &session.token
            } else {
                "invalid-token"
            })
        };
        write_frame_async(&mut socket, &build_auth_response_frame(2, payload).unwrap())
            .await
            .unwrap();
        read_frame_async(&mut socket).await.unwrap().kind
    };
    task.abort();
    result
}

#[tokio::test]
async fn standalone_tls_accepts_the_runtime_session_token() {
    assert_eq!(
        timeout(Duration::from_secs(20), authenticate(true, "bearer", true))
            .await
            .unwrap(),
        MessageKind::AuthOk
    );
}

#[tokio::test]
async fn standalone_tls_rejects_anonymous_when_runtime_auth_is_enabled() {
    assert_eq!(
        timeout(
            Duration::from_secs(20),
            authenticate(true, "anonymous", false)
        )
        .await
        .unwrap(),
        MessageKind::AuthFail
    );
}

#[tokio::test]
async fn standalone_tls_rejects_invalid_bearer_and_preserves_explicit_no_auth() {
    assert_eq!(
        timeout(Duration::from_secs(20), authenticate(true, "bearer", false))
            .await
            .unwrap(),
        MessageKind::AuthFail
    );
    assert_eq!(
        timeout(
            Duration::from_secs(20),
            authenticate(false, "anonymous", false)
        )
        .await
        .unwrap(),
        MessageKind::AuthOk
    );
}
