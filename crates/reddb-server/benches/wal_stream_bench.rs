use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use prost::Message;
use reddb_grpc_proto::{WalStreamChunk, WalStreamRecord};
use reddb_wire::replication::{
    WalStreamChunk as LegacyWalStreamChunk, WalStreamRecord as LegacyWalStreamRecord,
};

const FANOUT_REPLICAS: usize = 4;
const MEMORY_PROBE_ENV: &str = "REDDB_WAL_STREAM_MEMORY_PROBE";

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = System.alloc(layout);
        if !pointer.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(
                u64::try_from(layout.size()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = System.alloc_zeroed(layout);
        if !pointer.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(
                u64::try_from(layout.size()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = System.realloc(pointer, layout, new_size);
        if !new_pointer.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(
                u64::try_from(new_size).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        new_pointer
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
struct AllocationStats {
    count: u64,
    bytes: u64,
}

fn measure_allocations<T>(operation: impl FnOnce() -> T) -> (T, AllocationStats) {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Release);
    let result = operation();
    COUNT_ALLOCATIONS.store(false, Ordering::Release);
    (
        result,
        AllocationStats {
            count: ALLOCATION_COUNT.load(Ordering::Relaxed),
            bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        },
    )
}

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

fn decode_protobuf(bytes: &[u8]) -> WalStreamChunk {
    WalStreamChunk::decode(bytes).expect("benchmark protobuf chunk decodes")
}

fn decode_legacy(bytes: &[u8]) -> LegacyWalStreamChunk {
    LegacyWalStreamChunk::decode_json(bytes).expect("benchmark legacy chunk decodes")
}

fn percentile(samples: &mut [u128], percentile: usize) -> u128 {
    samples.sort_unstable();
    let index = (samples.len().saturating_sub(1) * percentile) / 100;
    samples[index]
}

fn report_codec_metrics(name: &str, protobuf: &WalStreamChunk, legacy: &LegacyWalStreamChunk) {
    let (protobuf_bytes, protobuf_encode_allocations) =
        measure_allocations(|| protobuf.encode_to_vec());
    let (legacy_bytes, legacy_encode_allocations) = measure_allocations(|| legacy.encode_json());
    let (_, protobuf_decode_allocations) = measure_allocations(|| decode_protobuf(&protobuf_bytes));
    let (_, legacy_decode_allocations) = measure_allocations(|| decode_legacy(&legacy_bytes));

    let mut protobuf_roundtrip = Vec::with_capacity(50);
    let mut legacy_roundtrip = Vec::with_capacity(50);
    for _ in 0..50 {
        let started = Instant::now();
        for _ in 0..FANOUT_REPLICAS {
            let encoded = protobuf.encode_to_vec();
            black_box(decode_protobuf(&encoded));
        }
        protobuf_roundtrip.push(started.elapsed().as_nanos());

        let started = Instant::now();
        for _ in 0..FANOUT_REPLICAS {
            let encoded = legacy.encode_json();
            black_box(decode_legacy(&encoded));
        }
        legacy_roundtrip.push(started.elapsed().as_nanos());
    }

    eprintln!(
        "[wal_stream_bench] case={name} protobuf_bytes={} legacy_json_bytes={} network_ratio={:.3} protobuf_encode_allocations={} protobuf_encode_allocated_bytes={} legacy_encode_allocations={} legacy_encode_allocated_bytes={} protobuf_decode_allocations={} protobuf_decode_allocated_bytes={} legacy_decode_allocations={} legacy_decode_allocated_bytes={} fanout={} protobuf_codec_p50_ns={} protobuf_codec_p95_ns={} legacy_codec_p50_ns={} legacy_codec_p95_ns={}",
        protobuf_bytes.len(),
        legacy_bytes.len(),
        protobuf_bytes.len() as f64 / legacy_bytes.len() as f64,
        protobuf_encode_allocations.count,
        protobuf_encode_allocations.bytes,
        legacy_encode_allocations.count,
        legacy_encode_allocations.bytes,
        protobuf_decode_allocations.count,
        protobuf_decode_allocations.bytes,
        legacy_decode_allocations.count,
        legacy_decode_allocations.bytes,
        FANOUT_REPLICAS,
        percentile(&mut protobuf_roundtrip, 50),
        percentile(&mut protobuf_roundtrip, 95),
        percentile(&mut legacy_roundtrip, 50),
        percentile(&mut legacy_roundtrip, 95),
    );
}

#[cfg(windows)]
fn peak_rss_bytes() -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>())
            .expect("process memory counter size fits in u32"),
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    let success = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    assert_ne!(success, 0, "GetProcessMemoryInfo failed");
    u64::try_from(counters.PeakWorkingSetSize).expect("peak RSS fits in u64")
}

#[cfg(unix)]
fn peak_rss_bytes() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(result, 0, "getrusage failed");
    let peak =
        u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).expect("peak RSS is non-negative");
    if cfg!(target_os = "macos") {
        peak
    } else {
        peak.saturating_mul(1024)
    }
}

fn run_memory_probe(transport: &str) -> ! {
    let payloads = payloads(16, 64 * 1024);
    let peak_before = peak_rss_bytes();
    let retained: Vec<Vec<u8>> = match transport {
        "protobuf" => {
            let chunk = protobuf_chunk(&payloads);
            (0..FANOUT_REPLICAS)
                .map(|_| chunk.encode_to_vec())
                .collect()
        }
        "legacy-json-hex" => {
            let chunk = legacy_chunk(&payloads);
            (0..FANOUT_REPLICAS).map(|_| chunk.encode_json()).collect()
        }
        _ => panic!("unknown memory probe transport: {transport}"),
    };
    let peak_after = peak_rss_bytes();
    eprintln!(
        "[wal_stream_memory] transport={transport} replicas={} retained_wire_bytes={} peak_rss_bytes={peak_after} peak_rss_growth_bytes={}",
        FANOUT_REPLICAS,
        retained.iter().map(Vec::len).sum::<usize>(),
        peak_after.saturating_sub(peak_before),
    );
    black_box(retained);
    std::process::exit(0);
}

fn report_isolated_memory() {
    let executable = std::env::current_exe().expect("benchmark executable path");
    for transport in ["protobuf", "legacy-json-hex"] {
        let output = Command::new(&executable)
            .env(MEMORY_PROBE_ENV, transport)
            .output()
            .expect("run isolated WAL memory probe");
        assert!(
            output.status.success(),
            "WAL memory probe failed for {transport}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
}

fn wal_stream_encoding(c: &mut Criterion) {
    if let Ok(transport) = std::env::var(MEMORY_PROBE_ENV) {
        run_memory_probe(&transport);
    }
    report_isolated_memory();

    let cases = [("small", 1_000, 128), ("large", 16, 64 * 1024)];
    let mut group = c.benchmark_group("wal-stream-encoding");
    group.sample_size(20);

    for (name, record_count, record_bytes) in cases {
        let payloads = payloads(record_count, record_bytes);
        let protobuf = protobuf_chunk(&payloads);
        let legacy = legacy_chunk(&payloads);
        report_codec_metrics(name, &protobuf, &legacy);

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

fn wal_stream_decoding(c: &mut Criterion) {
    let cases = [("small", 1_000, 128), ("large", 16, 64 * 1024)];
    let mut group = c.benchmark_group("wal-stream-decoding");
    group.sample_size(20);

    for (name, record_count, record_bytes) in cases {
        let payloads = payloads(record_count, record_bytes);
        let protobuf_bytes = protobuf_chunk(&payloads).encode_to_vec();
        let legacy_bytes = legacy_chunk(&payloads).encode_json();
        group.throughput(Throughput::Bytes(
            u64::try_from(record_count * record_bytes).expect("fixture size fits in u64"),
        ));
        group.bench_with_input(
            BenchmarkId::new("protobuf", name),
            &protobuf_bytes,
            |bench, bytes| bench.iter(|| black_box(decode_protobuf(bytes))),
        );
        group.bench_with_input(
            BenchmarkId::new("legacy-json-hex", name),
            &legacy_bytes,
            |bench, bytes| bench.iter(|| black_box(decode_legacy(bytes))),
        );
    }
    group.finish();
}

fn wal_stream_fanout(c: &mut Criterion) {
    let cases = [("small", 1_000, 128), ("large", 16, 64 * 1024)];
    let mut group = c.benchmark_group("wal-stream-four-replica-codec");
    group.sample_size(20);

    for (name, record_count, record_bytes) in cases {
        let payloads = payloads(record_count, record_bytes);
        let protobuf = protobuf_chunk(&payloads);
        let legacy = legacy_chunk(&payloads);
        group.throughput(Throughput::Bytes(
            u64::try_from(record_count * record_bytes * FANOUT_REPLICAS)
                .expect("fan-out fixture size fits in u64"),
        ));
        group.bench_with_input(
            BenchmarkId::new("protobuf", name),
            &protobuf,
            |bench, chunk| {
                bench.iter(|| {
                    for _ in 0..FANOUT_REPLICAS {
                        let encoded = chunk.encode_to_vec();
                        black_box(decode_protobuf(&encoded));
                    }
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("legacy-json-hex", name),
            &legacy,
            |bench, chunk| {
                bench.iter(|| {
                    for _ in 0..FANOUT_REPLICAS {
                        let encoded = chunk.encode_json();
                        black_box(decode_legacy(&encoded));
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    wal_stream_encoding,
    wal_stream_decoding,
    wal_stream_fanout
);
criterion_main!(benches);
