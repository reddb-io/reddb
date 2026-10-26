//! Preserve real gRPC negotiation when updating the runtime compression binding.
use reddb_server::grpc::{proto, GrpcServerOptions, RedDBGrpcServer};
use reddb_server::{RedDBOptions, RedDBRuntime};
use std::{net::TcpListener, sync::Arc, time::Duration};
use tonic::codec::CompressionEncoding;

async fn start_server() -> (String, tokio::task::JoinHandle<()>) {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let server = RedDBGrpcServer::with_options(
        runtime,
        GrpcServerOptions::default(),
        Arc::new(reddb_server::auth::store::AuthStore::new(
            reddb_server::auth::AuthConfig::default(),
        )),
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind gRPC listener");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let task = tokio::spawn(async move {
        server.serve_on(listener).await.expect("gRPC server serves");
    });
    (endpoint, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_preserves_zstd_gzip_and_identity_round_trips() {
    let (endpoint, task) = start_server().await;
    let payload = "grpc-compression-".repeat(4096);
    for (send, accept, expected) in [
        (None, None, None),
        (
            Some(CompressionEncoding::Zstd),
            Some(CompressionEncoding::Zstd),
            Some("zstd"),
        ),
        (
            Some(CompressionEncoding::Gzip),
            Some(CompressionEncoding::Zstd),
            Some("zstd"),
        ),
        (
            Some(CompressionEncoding::Gzip),
            Some(CompressionEncoding::Gzip),
            Some("gzip"),
        ),
    ] {
        let mut client = tokio::time::timeout(
            Duration::from_secs(10),
            proto::red_db_client::RedDbClient::connect(endpoint.clone()),
        )
        .await
        .expect("gRPC connect deadline")
        .expect("gRPC connects");
        if let Some(encoding) = send {
            client = client.send_compressed(encoding);
        }
        if let Some(encoding) = accept {
            client = client.accept_compressed(encoding);
        }
        let response = client
            .query(proto::QueryRequest {
                query: format!("SELECT '{payload}' AS payload"),
                entity_types: Vec::new(),
                capabilities: Vec::new(),
                params: Vec::new(),
            })
            .await
            .expect("compressed query round trip");
        assert_eq!(
            response
                .metadata()
                .get("grpc-encoding")
                .map(|value| value.to_str().expect("ASCII encoding")),
            expected
        );
        let reply = response.into_inner();
        assert_eq!(reply.record_count, 1);
        assert!(
            reply.result_json.contains(&payload),
            "compressed payload changed"
        );
    }
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_rejects_malformed_compressed_frames_and_keeps_serving() {
    use http_body_util::BodyExt;
    use tonic::codegen::Service;
    let (endpoint, task) = start_server().await;
    for encoding in ["zstd", "gzip"] {
        let mut channel = tonic::transport::Endpoint::from_shared(endpoint.clone())
            .expect("valid endpoint")
            .connect()
            .await
            .expect("connect raw gRPC client");
        std::future::poll_fn(|context| channel.poll_ready(context))
            .await
            .expect("channel ready");
        // A complete compressed gRPC frame with an invalid codec payload.
        let bytes = bytes::Bytes::from_static(&[1, 0, 0, 0, 4, b'b', b'a', b'd', b'!']);
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("{endpoint}/reddb.v1.RedDb/Query"))
            .header("content-type", "application/grpc")
            .header("te", "trailers")
            .header("grpc-encoding", encoding)
            .body(tonic::body::Body::new(http_body_util::Full::new(bytes)))
            .expect("valid raw HTTP request");
        let response = channel
            .call(request)
            .await
            .expect("malformed frame receives a gRPC error");
        let (headers, body) = response.into_parts();
        let body = body.collect().await.expect("collect error response");
        let trailers = body.trailers();
        let value = |name| {
            headers
                .headers
                .get(name)
                .or_else(|| trailers.and_then(|trailers| trailers.get(name)))
        };
        assert_eq!(value("grpc-status").expect("gRPC error status"), "13");
        assert!(value("grpc-message")
            .expect("decompression error message")
            .to_str()
            .expect("ASCII error")
            .to_lowercase()
            .contains("decompress"));
    }
    let mut client = proto::red_db_client::RedDbClient::connect(endpoint)
        .await
        .expect("server still accepts clients");
    let response = client
        .query(proto::QueryRequest {
            query: "SELECT 1 AS value".into(),
            entity_types: Vec::new(),
            capabilities: Vec::new(),
            params: Vec::new(),
        })
        .await
        .expect("valid RPC succeeds after malformed requests");
    assert_eq!(response.into_inner().record_count, 1);
    task.abort();
}
