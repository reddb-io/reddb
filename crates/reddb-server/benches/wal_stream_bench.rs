use std::hint::black_box;

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use prost::Message;
use reddb_grpc_proto::{WalStreamChunk, WalStreamRecord};
use reddb_wire::replication::{
    WalStreamChunk as LegacyWalStreamChunk, WalStreamRecord as LegacyWalStreamRecord,
};

fn payloads(record_count: usize, record_bytes: usize) -> Vec<Vec<u8>> {
    (0..record_count)
        .map(|index| vec![u8::try_from(index % 251).expect("modulo 251 fits in u8"); record_bytes])
        .collect()
}

fn protobuf_chunk(payloads: &[Vec<u8>]) -> WalStreamChunk {
    WalStreamChunk {
        version: 1,
        records: payloads
            .iter()
            .enumerate()
            .map(|(index, data)| WalStreamRecord {
                lsn: u64::try_from(index).expect("record index fits in u64") + 1,
                data: Bytes::copy_from_slice(data),
            })
            .collect(),
        current_lsn: u64::try_from(payloads.len()).expect("record count fits in u64"),
        oldest_available_lsn: Some(1),
        partial_resync: false,
        partial_resync_count: 0,
        needs_rebootstrap: false,
        invalidation_reason: None,
        catchup: None,
        primary_endpoints: Default::default(),
    }
}

fn legacy_chunk(payloads: &[Vec<u8>]) -> LegacyWalStreamChunk {
    LegacyWalStreamChunk {
        records: payloads
            .iter()
            .enumerate()
            .map(|(index, data)| LegacyWalStreamRecord {
                lsn: u64::try_from(index).expect("record index fits in u64") + 1,
                data: data.clone(),
            })
            .collect(),
        current_lsn: u64::try_from(payloads.len()).expect("record count fits in u64"),
        oldest_available_lsn: Some(1),
        partial_resync: false,
        partial_resync_count: 0,
        needs_rebootstrap: false,
        invalidation_reason: None,
        catchup: None,
    }
}

fn wal_stream_encoding(c: &mut Criterion) {
    let cases = [("small", 1_000, 128), ("large", 16, 64 * 1024)];
    let mut group = c.benchmark_group("wal-stream-encoding");
    group.sample_size(20);

    for (name, record_count, record_bytes) in cases {
        let payloads = payloads(record_count, record_bytes);
        let protobuf = protobuf_chunk(&payloads);
        let legacy = legacy_chunk(&payloads);
        let protobuf_bytes = protobuf.encode_to_vec().len();
        let legacy_bytes = legacy.encode_json().len();
        eprintln!(
            "[wal_stream_bench] case={name} payload_bytes={} protobuf_bytes={protobuf_bytes} legacy_json_bytes={legacy_bytes} network_ratio={:.3}",
            record_count * record_bytes,
            protobuf_bytes as f64 / legacy_bytes as f64,
        );

        group.throughput(Throughput::Bytes(
            u64::try_from(record_count * record_bytes).expect("fixture size fits in u64"),
        ));
        group.bench_with_input(
            BenchmarkId::new("protobuf", name),
            &protobuf,
            |bench, chunk| {
                bench.iter(|| black_box(chunk.encode_to_vec()));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("legacy-json-hex", name),
            &legacy,
            |bench, chunk| {
                bench.iter(|| black_box(chunk.encode_json()));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, wal_stream_encoding);
criterion_main!(benches);
