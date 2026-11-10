use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use futures_util::future::join_all;
use reddb_server::auth::{AuthConfig, AuthStore};
use reddb_server::grpc::proto::red_db_client::RedDbClient;
use reddb_server::grpc::proto::{JsonPayloadRequest, WalStreamRequest};
use reddb_server::{
    GrpcServerOptions, RedDBGrpcServer, RedDBOptions, RedDBRuntime, ReplicationConfig,
};
use reddb_wire::replication::{WalStreamChunk as LegacyWalStreamChunk, WalStreamOpen};
use tonic::transport::{Channel, Endpoint};

const REPLICA_COUNT: usize = 4;
const LATENCY_SAMPLES: usize = 20;

type Client = RedDbClient<Channel>;

struct RpcFixture {
    binary_clients: Vec<Client>,
    legacy_clients: Vec<Client>,
    binary_ids: Vec<String>,
    legacy_ids: Vec<String>,
    record_count: usize,
    payload_bytes: usize,
    server: tokio::task::JoinHandle<()>,
}

impl RpcFixture {
    async fn start(name: &str, record_count: usize, payload_bytes: usize) -> Self {
        // The Windows benchmark main thread has a small default stack. SQL
        // parser setup in an unoptimized build can exceed it for the 64 KiB
        // fixture, so isolate fixture construction from the measured RPC path.
        let primary = std::thread::Builder::new()
            .name(format!("wal-stream-{name}-setup"))
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let primary = RedDBRuntime::with_options(
                    RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
                )
                .expect("benchmark primary runtime");
                primary
                    .execute_query(
                        "CREATE TABLE wal_stream_rpc_bench (id INTEGER, payload TEXT)",
                    )
                    .expect("create benchmark table");
                for index in 0..record_count {
                    let payload = benchmark_payload(index, payload_bytes);
                    primary
                        .execute_query(&format!(
                            "INSERT INTO wal_stream_rpc_bench (id, payload) VALUES ({index}, '{payload}')"
                        ))
                        .expect("insert benchmark WAL record");
                }
                primary
            })
            .expect("spawn benchmark fixture setup")
            .join()
            .expect("benchmark fixture setup joins");

        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind benchmark gRPC listener");
        let address = listener.local_addr().expect("benchmark listener address");
        let server = RedDBGrpcServer::with_options(
            primary,
            GrpcServerOptions {
                bind_addr: address.to_string(),
                tls: None,
            },
            Arc::new(AuthStore::new(AuthConfig::default())),
        );
        let server = tokio::spawn(async move {
            server
                .serve_on(listener)
                .await
                .expect("benchmark gRPC server");
        });
        let endpoint = format!("http://{address}");
        let mut binary_clients = Vec::with_capacity(REPLICA_COUNT);
        let mut legacy_clients = Vec::with_capacity(REPLICA_COUNT);
        for _ in 0..REPLICA_COUNT {
            binary_clients.push(connect_client(&endpoint).await);
            legacy_clients.push(connect_client(&endpoint).await);
        }

        Self {
            binary_clients,
            legacy_clients,
            binary_ids: replica_ids(name, "binary"),
            legacy_ids: replica_ids(name, "legacy"),
            record_count,
            payload_bytes,
            server,
        }
    }

    async fn fetch_binary(&mut self) {
        let record_count = self.record_count;
        let pulls = self.binary_clients.iter_mut().zip(&self.binary_ids).map(
            |(client, replica_id)| async move {
                let mut stream = client
                    .stream_wal_records(WalStreamRequest {
                        version: 1,
                        since_lsn: 0,
                        max_count: u32::try_from(record_count)
                            .expect("benchmark record count fits in u32"),
                        replica_id: Some(replica_id.clone()),
                        term: 1,
                        await_data: false,
                        await_timeout_ms: 1,
                        routing_endpoints: Default::default(),
                        max_chunk_bytes: 1024 * 1024,
                    })
                    .await
                    .expect("open binary benchmark stream")
                    .into_inner();
                let mut received = 0;
                while let Some(chunk) = stream.message().await.expect("read binary benchmark chunk")
                {
                    received += chunk.records.len();
                }
                received
            },
        );
        for received in join_all(pulls).await {
            assert_eq!(received, record_count, "binary catch-up record count");
        }
    }

    async fn fetch_legacy(&mut self) {
        let record_count = self.record_count;
        let pulls = self.legacy_clients.iter_mut().zip(&self.legacy_ids).map(
            |(client, replica_id)| async move {
                let open = WalStreamOpen {
                    since_lsn: 0,
                    max_count: record_count,
                    replica_id: Some(replica_id.clone()),
                    term: 1,
                    await_data: false,
                    await_timeout_ms: 1,
                };
                let response = client
                    .pull_wal_records(JsonPayloadRequest {
                        payload_json: String::from_utf8(open.encode_json())
                            .expect("legacy benchmark request is UTF-8"),
                    })
                    .await
                    .expect("fetch legacy benchmark chunk")
                    .into_inner();
                LegacyWalStreamChunk::decode_json(response.payload.as_bytes())
                    .expect("decode legacy benchmark chunk")
                    .records
                    .len()
            },
        );
        for received in join_all(pulls).await {
            assert_eq!(received, record_count, "legacy catch-up record count");
        }
    }

    fn user_payload_bytes(&self) -> u64 {
        u64::try_from(self.record_count * self.payload_bytes * REPLICA_COUNT)
            .expect("benchmark payload size fits in u64")
    }
}

impl Drop for RpcFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn connect_client(endpoint: &str) -> Client {
    let endpoint = Endpoint::from_shared(endpoint.to_string())
        .expect("valid benchmark endpoint")
        .connect_timeout(Duration::from_secs(5));
    for _ in 0..100 {
        match endpoint.clone().connect().await {
            Ok(channel) => return RedDbClient::new(channel),
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    panic!("benchmark gRPC server did not accept connections");
}

fn replica_ids(case: &str, transport: &str) -> Vec<String> {
    (0..REPLICA_COUNT)
        .map(|index| format!("bench_{case}_{transport}_{index}"))
        .collect()
}

fn benchmark_payload(seed: usize, len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let alphabet_len = u64::try_from(ALPHABET.len()).expect("alphabet length fits in u64");
    let mut state = u64::try_from(seed).expect("fixture seed fits in u64") + 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let index =
                usize::try_from(state % alphabet_len).expect("alphabet index fits in usize");
            char::from(ALPHABET[index])
        })
        .collect()
}

fn percentile(samples: &mut [u128], percentile: usize) -> u128 {
    samples.sort_unstable();
    let index = (samples.len().saturating_sub(1) * percentile) / 100;
    samples[index]
}

fn print_latency(name: &str, transport: &str, fixture: &RpcFixture, samples: &mut [u128]) {
    let p50 = percentile(samples, 50);
    let p95 = percentile(samples, 95);
    let throughput_mib_s =
        fixture.user_payload_bytes() as f64 / (p50 as f64 / 1_000_000_000.0) / (1024.0 * 1024.0);
    eprintln!(
        "[wal_stream_rpc] case={name} transport={transport} replicas={REPLICA_COUNT} records_per_replica={} user_payload_bytes_per_replica={} catchup_p50_ns={p50} catchup_p95_ns={p95} p50_throughput_mib_s={throughput_mib_s:.3}",
        fixture.record_count,
        fixture.record_count * fixture.payload_bytes,
    );
}

fn report_latency_pair(runtime: &tokio::runtime::Runtime, name: &str, fixture: &mut RpcFixture) {
    runtime.block_on(async {
        fixture.fetch_binary().await;
        fixture.fetch_legacy().await;
    });
    let mut binary_samples = Vec::with_capacity(LATENCY_SAMPLES);
    let mut legacy_samples = Vec::with_capacity(LATENCY_SAMPLES);
    for sample in 0..LATENCY_SAMPLES {
        if sample % 2 == 0 {
            let started = Instant::now();
            runtime.block_on(fixture.fetch_binary());
            binary_samples.push(started.elapsed().as_nanos());
            let started = Instant::now();
            runtime.block_on(fixture.fetch_legacy());
            legacy_samples.push(started.elapsed().as_nanos());
        } else {
            let started = Instant::now();
            runtime.block_on(fixture.fetch_legacy());
            legacy_samples.push(started.elapsed().as_nanos());
            let started = Instant::now();
            runtime.block_on(fixture.fetch_binary());
            binary_samples.push(started.elapsed().as_nanos());
        }
    }
    print_latency(name, "protobuf", fixture, &mut binary_samples);
    print_latency(name, "legacy-json-hex", fixture, &mut legacy_samples);
}

fn wal_stream_rpc(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(REPLICA_COUNT)
        .enable_all()
        .build()
        .expect("benchmark Tokio runtime");

    for (name, record_count, payload_bytes) in [("small", 128, 128), ("large", 16, 64 * 1024)] {
        let mut fixture = runtime.block_on(RpcFixture::start(name, record_count, payload_bytes));
        report_latency_pair(&runtime, name, &mut fixture);

        let mut group = c.benchmark_group(format!("wal-stream-rpc-{name}"));
        group.sample_size(10);
        group.warm_up_time(Duration::from_millis(500));
        group.measurement_time(Duration::from_secs(2));
        group.throughput(Throughput::Bytes(fixture.user_payload_bytes()));
        group.bench_function(BenchmarkId::new("protobuf", REPLICA_COUNT), |bench| {
            bench.iter_custom(|iterations| {
                let started = Instant::now();
                runtime.block_on(async {
                    for _ in 0..iterations {
                        fixture.fetch_binary().await;
                    }
                });
                started.elapsed()
            });
        });
        group.bench_function(
            BenchmarkId::new("legacy-json-hex", REPLICA_COUNT),
            |bench| {
                bench.iter_custom(|iterations| {
                    let started = Instant::now();
                    runtime.block_on(async {
                        for _ in 0..iterations {
                            fixture.fetch_legacy().await;
                        }
                    });
                    started.elapsed()
                });
            },
        );
        group.finish();
    }
}

criterion_group!(benches, wal_stream_rpc);
criterion_main!(benches);
