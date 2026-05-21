/// Blob Cache benchmark suite — issue #190 / #149.
///
/// Covers all 8 workloads from bench/blob-cache/scenarios.md.
/// L1 is scaled to 8 MiB (default 256 MiB) so the suite finishes quickly
/// on any host; the relative comparisons are host-invariant.
///
/// Redis cells require a running Redis 7.4 instance:
///   export REDIS_NO_PERSIST_ADDR=127.0.0.1:6379
///   export REDIS_AOF_ADDR=127.0.0.1:6380
/// See bench/blob-cache/redis-up.sh to start them.
///
/// Without those env vars the Redis cells are silently skipped.
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use reddb_server::storage::cache::{
    BlobCache, BlobCacheConfig, BlobCachePut, CacheKey, CachePolicy, L2Compression, ResultCache,
};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

// ── constants ────────────────────────────────────────────────────────────────

/// L1 cap used across all scenarios (scaled down from the 256 MiB default so
/// the suite is fast on any CI host without invalidating the relative numbers).
const L1: usize = 8 * 1024 * 1024; // 8 MiB

const NS: &str = "bench";

// ── helpers ──────────────────────────────────────────────────────────────────

fn make_cache_l1_only(l1_bytes: usize) -> BlobCache {
    make_cache_l1_only_with_shards(l1_bytes, 64)
}

fn make_cache_l1_only_with_shards(l1_bytes: usize, shard_count: usize) -> BlobCache {
    BlobCache::new(
        BlobCacheConfig::builder()
            .l1_bytes_max(l1_bytes)
            .shard_count(shard_count)
            .max_namespaces(16)
            .l2_compression(L2Compression::Off)
            .try_build()
            .unwrap(),
    )
}

fn make_cache_with_l2(l1_bytes: usize, l2_path: &std::path::Path) -> BlobCache {
    BlobCache::new(
        BlobCacheConfig::builder()
            .l1_bytes_max(l1_bytes)
            .l2_bytes_max(4 * 1024 * 1024 * 1024)
            .l2_path(l2_path)
            .shard_count(64)
            .max_namespaces(16)
            .l2_compression(L2Compression::Off)
            .try_build()
            .unwrap(),
    )
}

fn key(i: usize) -> String {
    format!("k{i:010}")
}

fn payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i & 0xFF) as u8).collect()
}

enum Resp {
    Simple,
    Integer(i64),
    Bulk(Option<Vec<u8>>),
    Array(Vec<Resp>),
}

struct RedisBenchClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl RedisBenchClient {
    fn connect(addr: &str) -> io::Result<Self> {
        let writer = TcpStream::connect(addr)?;
        writer.set_nodelay(true)?;
        let reader_stream = writer.try_clone()?;
        Ok(Self {
            reader: BufReader::new(reader_stream),
            writer,
        })
    }

    fn command(&mut self, parts: &[&[u8]]) -> io::Result<Resp> {
        write!(self.writer, "*{}\r\n", parts.len())?;
        for part in parts {
            write!(self.writer, "${}\r\n", part.len())?;
            self.writer.write_all(part)?;
            self.writer.write_all(b"\r\n")?;
        }
        self.writer.flush()?;
        self.read_resp()
    }

    fn ping(&mut self) -> io::Result<()> {
        match self.command(&[b"PING"])? {
            Resp::Simple => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected PING response",
            )),
        }
    }

    fn flushdb(&mut self) -> io::Result<()> {
        match self.command(&[b"FLUSHDB"])? {
            Resp::Simple => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected FLUSHDB response",
            )),
        }
    }

    fn set(&mut self, key: &str, value: &[u8]) -> io::Result<()> {
        match self.command(&[b"SET", key.as_bytes(), value])? {
            Resp::Simple => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected SET response",
            )),
        }
    }

    fn sadd(&mut self, key: &str, member: &str) -> io::Result<()> {
        match self.command(&[b"SADD", key.as_bytes(), member.as_bytes()])? {
            Resp::Integer(_) => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected SADD response",
            )),
        }
    }

    fn get(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        match self.command(&[b"GET", key.as_bytes()])? {
            Resp::Bulk(value) => Ok(value),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected GET response",
            )),
        }
    }

    fn mget(&mut self, keys: &[String]) -> io::Result<usize> {
        let mut parts: Vec<&[u8]> = Vec::with_capacity(keys.len() + 1);
        parts.push(b"MGET");
        for key in keys {
            parts.push(key.as_bytes());
        }
        match self.command(&parts)? {
            Resp::Array(values) => Ok(values.len()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected MGET response",
            )),
        }
    }

    fn dbsize(&mut self) -> io::Result<i64> {
        match self.command(&[b"DBSIZE"])? {
            Resp::Integer(value) => Ok(value),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected DBSIZE response",
            )),
        }
    }

    fn eval_delete_tag_set(&mut self, set_key: &str) -> io::Result<i64> {
        let script = br#"
local members = redis.call('SMEMBERS', KEYS[1])
local count = 0
for _, key in ipairs(members) do
  count = count + redis.call('DEL', key)
end
redis.call('DEL', KEYS[1])
return count
"#;
        match self.command(&[b"EVAL", script, b"1", set_key.as_bytes()])? {
            Resp::Integer(value) => Ok(value),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected EVAL response",
            )),
        }
    }

    fn read_resp(&mut self) -> io::Result<Resp> {
        let mut kind = [0u8; 1];
        self.reader.read_exact(&mut kind)?;
        match kind[0] {
            b'+' => {
                self.read_line()?;
                Ok(Resp::Simple)
            }
            b'-' => {
                let line = self.read_line()?;
                Err(io::Error::other(line))
            }
            b':' => {
                let line = self.read_line()?;
                let value = line.parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid RESP integer")
                })?;
                Ok(Resp::Integer(value))
            }
            b'$' => {
                let line = self.read_line()?;
                let len: isize = line.parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid RESP bulk length")
                })?;
                if len < 0 {
                    return Ok(Resp::Bulk(None));
                }
                let mut value = vec![0u8; len as usize];
                self.reader.read_exact(&mut value)?;
                self.expect_crlf()?;
                Ok(Resp::Bulk(Some(value)))
            }
            b'*' => {
                let line = self.read_line()?;
                let len: isize = line.parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid RESP array length")
                })?;
                if len < 0 {
                    return Ok(Resp::Array(Vec::new()));
                }
                let mut values = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    values.push(self.read_resp()?);
                }
                Ok(Resp::Array(values))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown RESP frame type",
            )),
        }
    }

    fn read_line(&mut self) -> io::Result<String> {
        let mut line = Vec::new();
        self.reader.read_until(b'\n', &mut line)?;
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
        }
        String::from_utf8(line)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 RESP line"))
    }

    fn expect_crlf(&mut self) -> io::Result<()> {
        let mut crlf = [0u8; 2];
        self.reader.read_exact(&mut crlf)?;
        if crlf == *b"\r\n" {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing RESP CRLF",
            ))
        }
    }
}

fn redis_client(env_var: &str) -> Option<RedisBenchClient> {
    let addr = std::env::var(env_var).ok()?;
    let mut client = RedisBenchClient::connect(&addr).ok()?;
    client.ping().ok()?;
    Some(client)
}

// ── Workload 1 — hot-l1-hit ──────────────────────────────────────────────────
//
// 32 × 1 KB keys, warm in L1. Drives Arc<[u8]> clone path.
// Compares: BlobCache L1 | ResultCache | Redis GET single-shot | pipelined.

fn w1_hot_l1_hit(c: &mut Criterion) {
    const KEY_COUNT: usize = 32;
    const PAYLOAD_SIZE: usize = 1024; // 1 KB

    let cache = make_cache_l1_only(L1);
    let p = payload(PAYLOAD_SIZE);
    for i in 0..KEY_COUNT {
        cache.put(NS, key(i), BlobCachePut::new(p.clone())).unwrap();
    }

    let mut rc = ResultCache::new(L1);
    for i in 0..KEY_COUNT {
        rc.insert(CacheKey::new(key(i)), p.clone(), CachePolicy::default());
    }

    let mut g = c.benchmark_group("w1-hot-l1-hit");
    g.throughput(Throughput::Elements(1));

    g.bench_function("BlobCache-L1", |b| {
        let mut i = 0usize;
        b.iter(|| {
            black_box(cache.get(NS, &key(i % KEY_COUNT)));
            i += 1;
        });
    });

    g.bench_function("ResultCache", |b| {
        let mut i = 0usize;
        b.iter(|| {
            black_box(rc.get(&CacheKey::new(key(i % KEY_COUNT))));
            i += 1;
        });
    });

    if let Some(mut redis) = redis_client("REDIS_NO_PERSIST_ADDR") {
        redis.flushdb().unwrap();
        for i in 0..KEY_COUNT {
            redis.set(&key(i), &p).unwrap();
        }
        g.throughput(Throughput::Elements(1));
        g.bench_function("Redis-no-persist-GET", |b| {
            let mut i = 0usize;
            b.iter(|| {
                black_box(redis.get(&key(i % KEY_COUNT)).unwrap());
                i += 1;
            });
        });
    }

    if let Some(mut redis) = redis_client("REDIS_NO_PERSIST_ADDR") {
        redis.flushdb().unwrap();
        for i in 0..KEY_COUNT {
            redis.set(&key(i), &p).unwrap();
        }
        let mget_keys: Vec<String> = (0..KEY_COUNT).map(key).collect();
        g.throughput(Throughput::Elements(KEY_COUNT as u64));
        g.bench_function("Redis-no-persist-MGET-32", |b| {
            b.iter(|| {
                black_box(redis.mget(&mget_keys).unwrap());
            });
        });
    }

    if let Some(mut redis) = redis_client("REDIS_AOF_ADDR") {
        redis.flushdb().unwrap();
        for i in 0..KEY_COUNT {
            redis.set(&key(i), &p).unwrap();
        }
        g.throughput(Throughput::Elements(1));
        g.bench_function("Redis-aof-everysec-GET", |b| {
            let mut i = 0usize;
            b.iter(|| {
                black_box(redis.get(&key(i % KEY_COUNT)).unwrap());
                i += 1;
            });
        });
    }

    g.finish();
}

// ── Workload 2 — cold-l2-miss ────────────────────────────────────────────────
//
// L1 evicted, L2 hit. 32K × 16 KB keys (512 MiB working set > L1).
// Scaled down: 512 × 16 KB = 8 MiB = 1× L1, then narrow L1 to 1 MiB.

fn w2_cold_l2_miss(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    const KEY_COUNT: usize = 512;
    const PAYLOAD_SIZE: usize = 16 * 1024; // 16 KB

    // Populate with full-size cache (L1 + L2).
    let full_cache = make_cache_with_l2(L1, &tmp.path().join("cache.rdb"));
    let p = payload(PAYLOAD_SIZE);
    for i in 0..KEY_COUNT {
        full_cache
            .put(NS, key(i), BlobCachePut::new(p.clone()))
            .ok();
    }
    drop(full_cache);

    // Reopen with tiny L1 so reads come from L2.
    let cold_cache = make_cache_with_l2(1024, &tmp.path().join("cache.rdb"));

    let mut g = c.benchmark_group("w2-cold-l2-miss");
    g.throughput(Throughput::Elements(1));
    g.sample_size(50);

    g.bench_function("BlobCache-L2-hit", |b| {
        let mut i = 0usize;
        b.iter(|| {
            black_box(cold_cache.get(NS, &key(i % KEY_COUNT)));
            i += 1;
        });
    });

    if let Some(mut redis) = redis_client("REDIS_AOF_ADDR") {
        redis.flushdb().unwrap();
        for i in 0..KEY_COUNT {
            redis.set(&key(i), &p).unwrap();
        }
        g.bench_function("Redis-aof-everysec-GET", |b| {
            let mut i = 0usize;
            b.iter(|| {
                black_box(redis.get(&key(i % KEY_COUNT)).unwrap());
                i += 1;
            });
        });
    }

    g.finish();
}

// ── Workload 3 — cold-absent (synopsis effectiveness) ────────────────────────
//
// 100K keys, none written. Synopsis must skip L2 metadata reads.

fn w3_cold_absent(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    // Populate a few keys so the namespace + synopsis exist.
    let cache = make_cache_with_l2(L1, &tmp.path().join("cache.rdb"));
    let p = payload(64);
    for i in 0..100usize {
        cache.put(NS, key(i), BlobCachePut::new(p.clone())).unwrap();
    }

    let mut g = c.benchmark_group("w3-cold-absent");
    g.throughput(Throughput::Elements(1));

    g.bench_function("BlobCache-synopsis-miss", |b| {
        let mut i = 100usize; // keys that were never inserted
        b.iter(|| {
            black_box(cache.get(NS, &key(i)));
            i += 1;
        });
    });

    let mut rc = ResultCache::new(L1);
    for i in 0..100usize {
        rc.insert(CacheKey::new(key(i)), p.clone(), CachePolicy::default());
    }
    g.bench_function("ResultCache-miss", |b| {
        let mut i = 100usize;
        b.iter(|| {
            black_box(rc.get(&CacheKey::new(key(i))));
            i += 1;
        });
    });

    if let Some(mut redis) = redis_client("REDIS_NO_PERSIST_ADDR") {
        redis.flushdb().unwrap();
        for i in 0..100usize {
            redis.set(&key(i), &p).unwrap();
        }
        g.bench_function("Redis-no-persist-miss", |b| {
            let mut i = 100usize;
            b.iter(|| {
                black_box(redis.get(&key(i)).unwrap());
                i += 1;
            });
        });
    }

    g.finish();

    // Synopsis effectiveness: print l2_negative_skips / total misses.
    let s = cache.stats();
    let total_misses = s.misses();
    let synopsis_skips = s.l2_negative_skips();
    let skip_rate = if total_misses > 0 {
        synopsis_skips as f64 / total_misses as f64 * 100.0
    } else {
        0.0
    };
    eprintln!(
        "\n[w3 stats] synopsis skip-rate: {skip_rate:.1}% \
         (l2_negative_skips={synopsis_skips}, total_misses={total_misses})"
    );
}

// ── Workload 4 — large-blob-l2-hit (5 MiB) ──────────────────────────────────
//
// 5 MiB blob, L1 cold, L2 warm. Two L1-admission cells.

fn w4_large_blob_l2_hit(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    const BLOB_SIZE: usize = 5 * 1024 * 1024; // 5 MiB
    const KEY_COUNT: usize = 4;

    let full = make_cache_with_l2(64 * 1024 * 1024, &tmp.path().join("cache.rdb")); // 64 MiB L1
    let p = payload(BLOB_SIZE);
    for i in 0..KEY_COUNT {
        full.put(NS, key(i), BlobCachePut::new(p.clone())).ok();
    }
    drop(full);

    let cold = make_cache_with_l2(1024, &tmp.path().join("cache.rdb"));

    let mut g = c.benchmark_group("w4-large-blob-l2-hit");
    g.throughput(Throughput::Bytes(BLOB_SIZE as u64));
    g.sample_size(20);
    g.measurement_time(Duration::from_secs(10));

    g.bench_function("BlobCache-L2-hit-5MiB", |b| {
        let mut i = 0usize;
        b.iter(|| {
            black_box(cold.get(NS, &key(i % KEY_COUNT)));
            i += 1;
        });
    });

    if let Some(mut redis) = redis_client("REDIS_NO_PERSIST_ADDR") {
        redis.flushdb().unwrap();
        for i in 0..KEY_COUNT {
            redis.set(&key(i), &p).unwrap();
        }
        g.bench_function("Redis-no-persist-GET-5MiB", |b| {
            let mut i = 0usize;
            b.iter(|| {
                black_box(redis.get(&key(i % KEY_COUNT)).unwrap());
                i += 1;
            });
        });
    }

    if let Some(mut redis) = redis_client("REDIS_AOF_ADDR") {
        redis.flushdb().unwrap();
        for i in 0..KEY_COUNT {
            redis.set(&key(i), &p).unwrap();
        }
        g.bench_function("Redis-aof-everysec-GET-5MiB", |b| {
            let mut i = 0usize;
            b.iter(|| {
                black_box(redis.get(&key(i % KEY_COUNT)).unwrap());
                i += 1;
            });
        });
    }

    g.finish();
}

// ── Workload 5 — namespace-flush ─────────────────────────────────────────────
//
// O(1) generation bump on the foreground flush call.

fn w5_namespace_flush(c: &mut Criterion) {
    const KEY_COUNT: usize = 1000; // scaled from 50K
    const PAYLOAD_SIZE: usize = 4 * 1024; // 4 KB

    let cache = make_cache_l1_only(L1);
    let p = payload(PAYLOAD_SIZE);

    let mut g = c.benchmark_group("w5-namespace-flush");
    g.sample_size(50);

    g.bench_function("BlobCache-generation-bump", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Repopulate
                for i in 0..KEY_COUNT {
                    cache.put(NS, key(i), BlobCachePut::new(p.clone())).ok();
                }
                let t = Instant::now();
                black_box(cache.invalidate_namespace(NS));
                total += t.elapsed();
            }
            total
        });
    });

    if let Some(mut redis) = redis_client("REDIS_NO_PERSIST_ADDR") {
        g.bench_function("Redis-no-persist-FLUSHDB", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    redis.flushdb().unwrap();
                    for i in 0..KEY_COUNT {
                        redis.set(&key(i), &p).unwrap();
                    }
                    let t = Instant::now();
                    redis.flushdb().unwrap();
                    total += t.elapsed();
                }
                total
            });
        });
    }

    if let Some(mut redis) = redis_client("REDIS_NO_PERSIST_ADDR") {
        g.bench_function("Redis-no-persist-SCAN-DEL-prefix", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    redis.flushdb().unwrap();
                    for i in 0..KEY_COUNT {
                        redis.set(&format!("{NS}:{}", key(i)), &p).unwrap();
                    }
                    let t = Instant::now();
                    for i in 0..KEY_COUNT {
                        redis
                            .command(&[b"DEL", format!("{NS}:{}", key(i)).as_bytes()])
                            .unwrap();
                    }
                    total += t.elapsed();
                }
                total
            });
        });
    }

    g.finish();
}

// ── Workload 6 — dependency-invalidation ─────────────────────────────────────
//
// 25% of entries carry "table:users" dep. Measure invalidate_dependencies.

fn w6_dependency_invalidation(c: &mut Criterion) {
    const KEY_COUNT: usize = 1000; // scaled from 100K
    const PAYLOAD_SIZE: usize = 4 * 1024;

    let cache = make_cache_l1_only(L1);
    let p = payload(PAYLOAD_SIZE);

    let mut rc = ResultCache::new(L1);

    // Tracks the last observed invalidated-entry count across iter_custom calls.
    let last_invalidated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let last_inv_ref = last_invalidated.clone();

    let mut g = c.benchmark_group("w6-dependency-invalidation");
    g.sample_size(30);

    g.bench_function("BlobCache-dep-tag", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Repopulate with 25% tagged
                for i in 0..KEY_COUNT {
                    let put = if i % 4 == 0 {
                        BlobCachePut::new(p.clone()).with_dependencies(["table:users"])
                    } else {
                        BlobCachePut::new(p.clone())
                    };
                    cache.put(NS, key(i), put).ok();
                }
                let t = Instant::now();
                let count = cache.invalidate_dependencies(NS, &["table:users"]);
                total += t.elapsed();
                last_inv_ref.store(count, std::sync::atomic::Ordering::Relaxed);
            }
            total
        });
    });

    g.bench_function("ResultCache-invalidate-deps", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                for i in 0..KEY_COUNT {
                    let policy = if i % 4 == 0 {
                        CachePolicy::default().depends_on(&["table:users"])
                    } else {
                        CachePolicy::default()
                    };
                    rc.insert(CacheKey::new(key(i)), p.clone(), policy);
                }
                let t = Instant::now();
                rc.invalidate_by_dependency("table:users");
                total += t.elapsed();
            }
            total
        });
    });

    if let Some(mut redis) = redis_client("REDIS_NO_PERSIST_ADDR") {
        g.bench_function("Redis-no-persist-Lua-tag-set-sweep", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    redis.flushdb().unwrap();
                    for i in 0..KEY_COUNT {
                        let redis_key = format!("{NS}:{}", key(i));
                        redis.set(&redis_key, &p).unwrap();
                        if i % 4 == 0 {
                            redis.sadd("dep:table:users", &redis_key).unwrap();
                        }
                    }
                    let t = Instant::now();
                    black_box(redis.eval_delete_tag_set("dep:table:users").unwrap());
                    total += t.elapsed();
                }
                total
            });
        });
    }

    g.finish();

    let invalidated = last_invalidated.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "\n[w6 stats] BlobCache invalidated_count (last iter, 25% of {KEY_COUNT}): {invalidated}"
    );
}

// ── Workload 7 — restart-warm-cache ──────────────────────────────────────────
//
// Write 200K × 8 KB keys to L2, drop BlobCache, reopen, measure open + first hit.
// Scaled: 128 × 8 KB = 1 MiB so the bench doesn't require minutes.

fn w7_restart_warm_cache(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    const KEY_COUNT: usize = 128;
    const PAYLOAD_SIZE: usize = 8 * 1024; // 8 KB

    // Populate phase.
    {
        let cache = make_cache_with_l2(L1, &tmp.path().join("cache.rdb"));
        let p = payload(PAYLOAD_SIZE);
        for i in 0..KEY_COUNT {
            cache.put(NS, key(i), BlobCachePut::new(p.clone())).ok();
        }
    } // drop flushes L2

    let l2_path = tmp.path().join("cache.rdb");
    let check = make_cache_with_l2(L1, &l2_path);
    let mut reachable = 0usize;
    for i in 0..KEY_COUNT {
        if check.get(NS, &key(i)).is_some() {
            reachable += 1;
        }
    }
    drop(check);
    eprintln!("\n[w7 stats] entries reachable post-restart: {reachable}/{KEY_COUNT}");

    let mut g = c.benchmark_group("w7-restart-warm-cache");
    g.sample_size(20);
    g.measurement_time(Duration::from_secs(10));

    g.bench_function("BlobCache-reopen-first-hit", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let t = Instant::now();
                let reopened = make_cache_with_l2(L1, &l2_path);
                black_box(reopened.get(NS, &key(0)));
                total += t.elapsed();
            }
            total
        });
    });

    g.finish();
}

// ── Workload 8 — mixed-blob admission ────────────────────────────────────────
//
// Mix of 1 KB (70%) / 100 KB (25%) / 5 MiB (5%) blobs at WS = 0.5, 1.0, 2.0 × L1.
// This is the SIEVE vs W-TinyLFU oracle (W-TinyLFU not yet flagged → n/a row).

fn w8_mixed_blob_admission(c: &mut Criterion) {
    // Key counts sized so total bytes ≈ target WS multiple of L1.
    // 1 KB × 70% + 100 KB × 25% + 5 MiB × 5% = 0.7 + 25 + 262 ≈ 288 KB avg per slot
    // We approximate with scaled L1=8 MiB.
    // WS=0.5: total_bytes = 4 MiB → ops ~14 keys
    // WS=1.0: total_bytes = 8 MiB → ops ~28 keys
    // WS=2.0: total_bytes = 16 MiB → ops ~56 keys
    let workloads: &[(&str, usize)] = &[
        ("WS-0.5xL1", L1 / 2),
        ("WS-1.0xL1", L1),
        ("WS-2.0xL1", L1 * 2),
    ];

    let small_payload = payload(1024);
    let medium_payload = payload(100 * 1024);
    let large_payload = payload(1024 * 1024); // scaled from 5 MiB → 1 MiB

    let mut g = c.benchmark_group("w8-mixed-blob-admission");
    g.sample_size(20);

    for (label, ws_bytes) in workloads {
        // Build a mixed key set whose total size ~= ws_bytes.
        let small_count = (ws_bytes * 70 / 100) / 1024;
        let medium_count = (ws_bytes * 25 / 100) / (100 * 1024);
        let large_count = (ws_bytes * 5 / 100) / (1024 * 1024);
        let total_keys = (small_count + medium_count + large_count).max(1);

        g.bench_with_input(
            BenchmarkId::new("BlobCache-SIEVE-put-get", label),
            label,
            |b, _| {
                let cache = make_cache_l1_only(L1);
                // Warm fill
                for i in 0..total_keys {
                    let put = if i < small_count {
                        BlobCachePut::new(small_payload.clone())
                    } else if i < small_count + medium_count {
                        BlobCachePut::new(medium_payload.clone())
                    } else {
                        BlobCachePut::new(large_payload.clone())
                    };
                    cache.put(NS, key(i), put).ok();
                }
                let mut i = 0usize;
                b.iter(|| {
                    // 80/20 read/write mix
                    if i.is_multiple_of(5) {
                        let idx = i % total_keys;
                        let put = if idx < small_count {
                            BlobCachePut::new(small_payload.clone())
                        } else if idx < small_count + medium_count {
                            BlobCachePut::new(medium_payload.clone())
                        } else {
                            BlobCachePut::new(large_payload.clone())
                        };
                        cache.put(NS, key(idx), put).ok();
                    } else {
                        black_box(cache.get(NS, &key(i % total_keys)));
                    }
                    i += 1;
                });
            },
        );
    }

    if let Some(mut redis) = redis_client("REDIS_NO_PERSIST_ADDR") {
        let ws_bytes = L1 * 2;
        let small_count = (ws_bytes * 70 / 100) / 1024;
        let medium_count = (ws_bytes * 25 / 100) / (100 * 1024);
        let large_count = (ws_bytes * 5 / 100) / (1024 * 1024);
        let total_keys = (small_count + medium_count + large_count).max(1);

        g.bench_function("Redis-no-persist-allkeys-lru-WS-2.0xL1", |b| {
            let mut i = 0usize;
            redis.flushdb().unwrap();
            for key_idx in 0..total_keys {
                let put = if key_idx < small_count {
                    &small_payload
                } else if key_idx < small_count + medium_count {
                    &medium_payload
                } else {
                    &large_payload
                };
                redis.set(&key(key_idx), put).unwrap();
            }
            b.iter(|| {
                if i.is_multiple_of(5) {
                    let idx = i % total_keys;
                    let put = if idx < small_count {
                        &small_payload
                    } else if idx < small_count + medium_count {
                        &medium_payload
                    } else {
                        &large_payload
                    };
                    redis.set(&key(idx), put).unwrap();
                } else {
                    black_box(redis.get(&key(i % total_keys)).unwrap());
                }
                i += 1;
            });
        });
    }

    g.finish();

    // Hit-rate + eviction measurement (standalone, not folded into criterion timing).
    // Creates a fresh cache per WS size, runs 50K mixed ops, reads stats.
    eprintln!("\n[w8 hit-rate stats]");
    for (label, ws_bytes) in workloads {
        let small_count = (ws_bytes * 70 / 100) / 1024;
        let medium_count = (ws_bytes * 25 / 100) / (100 * 1024);
        let large_count = (ws_bytes * 5 / 100) / (1024 * 1024);
        let total_keys = (small_count + medium_count + large_count).max(1);

        let cache = make_cache_l1_only(L1);
        // Warm fill
        for i in 0..total_keys {
            let put = if i < small_count {
                BlobCachePut::new(small_payload.clone())
            } else if i < small_count + medium_count {
                BlobCachePut::new(medium_payload.clone())
            } else {
                BlobCachePut::new(large_payload.clone())
            };
            cache.put(NS, key(i), put).ok();
        }
        // 50K mixed ops (80/20 read/write)
        for i in 0..50_000usize {
            if i % 5 == 0 {
                let idx = i % total_keys;
                let put = if idx < small_count {
                    BlobCachePut::new(small_payload.clone())
                } else if idx < small_count + medium_count {
                    BlobCachePut::new(medium_payload.clone())
                } else {
                    BlobCachePut::new(large_payload.clone())
                };
                cache.put(NS, key(idx), put).ok();
            } else {
                black_box(cache.get(NS, &key(i % total_keys)));
            }
        }
        let s = cache.stats();
        let total_ops = s.hits() + s.misses();
        let hit_rate = if total_ops > 0 {
            s.hits() as f64 / total_ops as f64 * 100.0
        } else {
            0.0
        };
        eprintln!(
            "  SIEVE {label}: hit-rate={hit_rate:.1}% hits={} misses={} evictions={}",
            s.hits(),
            s.misses(),
            s.evictions()
        );
    }

    if let Some(mut redis) = redis_client("REDIS_NO_PERSIST_ADDR") {
        let ws_bytes = L1 * 2;
        let small_count = (ws_bytes * 70 / 100) / 1024;
        let medium_count = (ws_bytes * 25 / 100) / (100 * 1024);
        let large_count = (ws_bytes * 5 / 100) / (1024 * 1024);
        let total_keys = (small_count + medium_count + large_count).max(1);
        redis.flushdb().unwrap();
        for i in 0..total_keys {
            let put = if i < small_count {
                &small_payload
            } else if i < small_count + medium_count {
                &medium_payload
            } else {
                &large_payload
            };
            redis.set(&key(i), put).unwrap();
        }
        let mut hits = 0usize;
        let mut misses = 0usize;
        for i in 0..50_000usize {
            if i % 5 == 0 {
                let idx = i % total_keys;
                let put = if idx < small_count {
                    &small_payload
                } else if idx < small_count + medium_count {
                    &medium_payload
                } else {
                    &large_payload
                };
                redis.set(&key(idx), put).unwrap();
            } else if redis.get(&key(i % total_keys)).unwrap().is_some() {
                hits += 1;
            } else {
                misses += 1;
            }
        }
        let total_gets = hits + misses;
        let hit_rate = if total_gets > 0 {
            hits as f64 / total_gets as f64 * 100.0
        } else {
            0.0
        };
        let entries = redis.dbsize().unwrap_or_default();
        let evictions = total_keys.saturating_sub(entries as usize);
        eprintln!(
            "  Redis allkeys-lru WS-2.0xL1: hit-rate={hit_rate:.1}% hits={hits} misses={misses} evictions={evictions}"
        );
    }
}

// ── Workload 9 — shard insert/remove slot-index (#225) ──────────────────────
//
// Single-shard insert + exact-key invalidation isolates the Shard::insert /
// Shard::remove path. Before #225, both operations removed keys from
// Vec<BlobCacheKey> via order.iter().position(...); this workload is the
// before/after comparison point for N=10K and N=100K.

fn w9_shard_insert_remove_slot_index(c: &mut Criterion) {
    let mut g = c.benchmark_group("w9-shard-insert-remove-slot-index");
    g.sample_size(10);
    g.measurement_time(Duration::from_secs(5));

    for item_count in [10_000usize, 100_000usize] {
        let keys: Vec<String> = (0..item_count).map(key).collect();
        let p = payload(1);
        let l1_bytes = item_count * 2;

        g.throughput(Throughput::Elements((item_count * 2) as u64));
        g.bench_with_input(
            BenchmarkId::new("BlobCache-single-shard-put-invalidate", item_count),
            &item_count,
            |b, _| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let cache = make_cache_l1_only_with_shards(l1_bytes, 1);
                        let t = Instant::now();
                        for key in &keys {
                            cache
                                .put(NS, key.clone(), BlobCachePut::new(p.clone()))
                                .unwrap();
                        }
                        for key in &keys {
                            black_box(cache.invalidate_key(NS, key));
                        }
                        total += t.elapsed();
                    }
                    total
                });
            },
        );
    }

    g.finish();
}

// ── criterion wiring ─────────────────────────────────────────────────────────

criterion_group!(
    benches,
    w1_hot_l1_hit,
    w2_cold_l2_miss,
    w3_cold_absent,
    w4_large_blob_l2_hit,
    w5_namespace_flush,
    w6_dependency_invalidation,
    w7_restart_warm_cache,
    w8_mixed_blob_admission,
    w9_shard_insert_remove_slot_index,
);
criterion_main!(benches);
