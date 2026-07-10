//! Cache topology spike benchmark for issue #1970.
//!
//! Re-runnable command:
//!
//! REDDB_CACHE_TOPOLOGY_COMMIT=$(git rev-parse --short HEAD) \
//! REDDB_CACHE_TOPOLOGY_REPORT=docs/perf/cache-topology-spike-2026-07-10.md \
//! cargo bench -p reddb-io-server --bench cache_topology_spike_bench -- --nocapture

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use reddb_server::storage::cache::{
    BlobCache, BlobCacheConfig, BlobCachePut, L2Compression, DEFAULT_BLOB_L2_BYTES_MAX,
};
use reddb_server::storage::engine::{Page, PageCache, PageType};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const NS: &str = "topology";
const SLOT_BYTES: usize = 1024;
const L1_SLOTS: usize = 64;
const L1_BYTES: usize = L1_SLOTS * SLOT_BYTES;
const WORKING_SET: usize = L1_SLOTS * 3;
const MEASURED_OPS: usize = 2_000;
const ALLOCATION_PROBE_OPS: usize = 1_000;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
enum Workload {
    PointReadHotL1,
    PointReadZipfianL2,
    ColdScan,
    MixedWriteHeavy,
}

impl Workload {
    fn all() -> [Self; 4] {
        [
            Self::PointReadHotL1,
            Self::PointReadZipfianL2,
            Self::ColdScan,
            Self::MixedWriteHeavy,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            Self::PointReadHotL1 => "point-read-hot-l1",
            Self::PointReadZipfianL2 => "point-read-zipfian-l2",
            Self::ColdScan => "cold-scan",
            Self::MixedWriteHeavy => "mixed-write-heavy",
        }
    }
}

#[derive(Clone, Copy)]
enum Candidate {
    BaselineShipped,
    UnifiedSlotArena,
    PromoteOnSecondHit,
}

impl Candidate {
    fn all() -> [Self; 3] {
        [
            Self::BaselineShipped,
            Self::UnifiedSlotArena,
            Self::PromoteOnSecondHit,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            Self::BaselineShipped => "baseline-shipped",
            Self::UnifiedSlotArena => "unified-slot-arena",
            Self::PromoteOnSecondHit => "promote-on-second-hit",
        }
    }

    fn hypothesis(self) -> &'static str {
        match self {
            Self::BaselineShipped => {
                "Separate shipped SIEVE page cache plus Blob L1/L2 should remain the control."
            }
            Self::UnifiedSlotArena => {
                "One fixed-slot arena should remove hit-path allocation and improve L1-heavy reads."
            }
            Self::PromoteOnSecondHit => {
                "Promoting only after a second L2 hit should reduce scan churn under eviction pressure."
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Op {
    Read(usize),
    Write(usize),
}

#[derive(Default, Clone, Copy)]
struct TopologyStats {
    l1_hits: u64,
    l2_hits: u64,
    misses: u64,
    evictions: u64,
    writes: u64,
}

#[derive(Clone)]
struct MetricRow {
    workload: &'static str,
    candidate: &'static str,
    p50_ns: u128,
    p99_ns: u128,
    ops_per_sec: f64,
    allocations_per_op: f64,
    stats: TopologyStats,
    disqualified: bool,
}

struct BaselineTopology {
    _tmp: tempfile::TempDir,
    page_cache: PageCache,
    blob_cache: BlobCache,
    keys: Vec<String>,
}

impl BaselineTopology {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir for blob L2");
        let l2_path = tmp.path().join("cache.rdb");
        let blob_cache = BlobCache::new(
            BlobCacheConfig::builder()
                .l1_bytes_max(L1_BYTES)
                .l2_bytes_max(DEFAULT_BLOB_L2_BYTES_MAX)
                .l2_path(&l2_path)
                .shard_count(8)
                .max_namespaces(8)
                .l2_compression(L2Compression::Off)
                .try_build()
                .expect("baseline blob cache config"),
        );
        let page_cache = PageCache::new(L1_SLOTS);
        let payload = payload();
        let mut keys = Vec::with_capacity(WORKING_SET * 2);
        for idx in 0..WORKING_SET {
            let key = format!("k{idx:06}");
            blob_cache
                .put(NS, key.as_str(), BlobCachePut::new(payload.clone()))
                .expect("populate blob cache");
            keys.push(key);
        }
        for page_id in 0..L1_SLOTS as u32 {
            page_cache.insert(page_id, Page::new(PageType::BTreeLeaf, page_id));
        }
        Self {
            _tmp: tmp,
            page_cache,
            blob_cache,
            keys,
        }
    }

    fn read(&mut self, idx: usize) -> bool {
        if idx % 4 == 0 {
            self.page_cache.get((idx % WORKING_SET) as u32).is_some()
        } else {
            self.blob_cache
                .get(NS, self.keys[idx % WORKING_SET].as_str())
                .is_some()
        }
    }

    fn write(&mut self, idx: usize) {
        let key_idx = idx % WORKING_SET;
        self.blob_cache
            .put(
                NS,
                self.keys[key_idx].as_str(),
                BlobCachePut::new(payload_for(idx)),
            )
            .expect("baseline write");
    }

    fn stats_since(&self, before_blob_l2_reads: u64, before_page_hits: u64) -> TopologyStats {
        let blob = self.blob_cache.stats();
        let page = self.page_cache.stats();
        let l2_hits = blob
            .l2_metadata_reads()
            .saturating_sub(before_blob_l2_reads);
        let page_hits = page.hits.saturating_sub(before_page_hits);
        let blob_hits = blob.hits();
        TopologyStats {
            l1_hits: page_hits + blob_hits.saturating_sub(l2_hits),
            l2_hits,
            misses: blob.misses() + page.misses,
            evictions: blob.evictions() + page.evictions,
            writes: blob.insertions(),
        }
    }

    fn raw_l2_reads(&self) -> u64 {
        self.blob_cache.stats().l2_metadata_reads()
    }

    fn raw_page_hits(&self) -> u64 {
        self.page_cache.stats().hits
    }
}

#[derive(Clone, Copy)]
struct Slot {
    key: u64,
    value: [u8; SLOT_BYTES],
    visited: bool,
    occupied: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            key: 0,
            value: [0; SLOT_BYTES],
            visited: false,
            occupied: false,
        }
    }
}

struct SlotArenaTopology {
    slots: Vec<Slot>,
    index: HashMap<u64, usize>,
    l2_present: Vec<bool>,
    second_hit_seen: Vec<bool>,
    hand: usize,
    promote_on_second_hit: bool,
    stats: TopologyStats,
}

impl SlotArenaTopology {
    fn new(promote_on_second_hit: bool) -> Self {
        let mut topology = Self {
            slots: vec![Slot::default(); L1_SLOTS],
            index: HashMap::with_capacity(L1_SLOTS * 2),
            l2_present: vec![true; WORKING_SET * 2],
            second_hit_seen: vec![false; WORKING_SET * 2],
            hand: 0,
            promote_on_second_hit,
            stats: TopologyStats::default(),
        };
        for key in 0..L1_SLOTS {
            topology.install(key);
        }
        topology.stats = TopologyStats::default();
        topology
    }

    fn read(&mut self, idx: usize) -> bool {
        let key = idx % WORKING_SET;
        if let Some(&slot_idx) = self.index.get(&(key as u64)) {
            self.slots[slot_idx].visited = true;
            self.stats.l1_hits += 1;
            black_box(&self.slots[slot_idx].value);
            return true;
        }
        if self.l2_present[key] {
            self.stats.l2_hits += 1;
            if self.should_promote(key) {
                self.install(key);
            }
            return true;
        }
        self.stats.misses += 1;
        false
    }

    fn write(&mut self, idx: usize) {
        let key = idx % self.l2_present.len();
        self.l2_present[key] = true;
        self.install(key);
        self.stats.writes += 1;
    }

    fn stats(&self) -> TopologyStats {
        self.stats
    }

    fn should_promote(&mut self, key: usize) -> bool {
        if !self.promote_on_second_hit {
            return true;
        }
        if self.second_hit_seen[key] {
            self.second_hit_seen[key] = false;
            true
        } else {
            self.second_hit_seen[key] = true;
            false
        }
    }

    fn install(&mut self, key: usize) {
        let key = key as u64;
        if let Some(&slot_idx) = self.index.get(&key) {
            self.slots[slot_idx].visited = true;
            return;
        }
        let slot_idx = self.victim_slot();
        if self.slots[slot_idx].occupied {
            self.index.remove(&self.slots[slot_idx].key);
            self.stats.evictions += 1;
        }
        self.slots[slot_idx] = Slot {
            key,
            value: payload_array(key),
            visited: false,
            occupied: true,
        };
        self.index.insert(key, slot_idx);
    }

    fn victim_slot(&mut self) -> usize {
        loop {
            let idx = self.hand;
            self.hand = (self.hand + 1) % self.slots.len();
            if !self.slots[idx].occupied || !self.slots[idx].visited {
                return idx;
            }
            self.slots[idx].visited = false;
        }
    }
}

enum Topology {
    Baseline(BaselineTopology),
    SlotArena(SlotArenaTopology),
}

impl Topology {
    fn new(candidate: Candidate) -> Self {
        match candidate {
            Candidate::BaselineShipped => Self::Baseline(BaselineTopology::new()),
            Candidate::UnifiedSlotArena => Self::SlotArena(SlotArenaTopology::new(false)),
            Candidate::PromoteOnSecondHit => Self::SlotArena(SlotArenaTopology::new(true)),
        }
    }

    fn read(&mut self, idx: usize) -> bool {
        match self {
            Self::Baseline(topology) => topology.read(idx),
            Self::SlotArena(topology) => topology.read(idx),
        }
    }

    fn write(&mut self, idx: usize) {
        match self {
            Self::Baseline(topology) => topology.write(idx),
            Self::SlotArena(topology) => topology.write(idx),
        }
    }

    fn stats(&self, before_blob_l2_reads: u64, before_page_hits: u64) -> TopologyStats {
        match self {
            Self::Baseline(topology) => {
                topology.stats_since(before_blob_l2_reads, before_page_hits)
            }
            Self::SlotArena(topology) => topology.stats(),
        }
    }

    fn raw_l2_reads(&self) -> u64 {
        match self {
            Self::Baseline(topology) => topology.raw_l2_reads(),
            Self::SlotArena(_) => 0,
        }
    }

    fn raw_page_hits(&self) -> u64 {
        match self {
            Self::Baseline(topology) => topology.raw_page_hits(),
            Self::SlotArena(_) => 0,
        }
    }
}

fn cache_topology_spike(c: &mut Criterion) {
    let rows = run_matrix();
    assert_boundary_crossing(&rows);
    maybe_write_report(&rows);

    let mut group = c.benchmark_group("cache-topology-spike");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(1));
    for workload in Workload::all() {
        let ops = ops_for(workload, MEASURED_OPS);
        for candidate in Candidate::all() {
            group.bench_with_input(
                BenchmarkId::new(workload.name(), candidate.name()),
                &(workload, candidate),
                |b, _| {
                    b.iter(|| {
                        let mut topology = Topology::new(candidate);
                        for op in &ops {
                            apply_op(&mut topology, *op);
                        }
                    });
                },
            );
        }
    }
    group.finish();
}

fn run_matrix() -> Vec<MetricRow> {
    let mut rows = Vec::new();
    for workload in Workload::all() {
        let ops = ops_for(workload, MEASURED_OPS);
        for candidate in Candidate::all() {
            rows.push(measure(workload, candidate, &ops));
        }
    }
    print_rows(&rows);
    rows
}

fn measure(workload: Workload, candidate: Candidate, ops: &[Op]) -> MetricRow {
    let mut topology = Topology::new(candidate);
    let before_blob_l2_reads = topology.raw_l2_reads();
    let before_page_hits = topology.raw_page_hits();
    let mut samples = Vec::with_capacity(ops.len());
    let started = Instant::now();
    for op in ops {
        let op_started = Instant::now();
        apply_op(&mut topology, *op);
        samples.push(op_started.elapsed().as_nanos());
    }
    let elapsed = started.elapsed();
    samples.sort_unstable();
    let p50_ns = percentile(&samples, 50);
    let p99_ns = percentile(&samples, 99);
    let ops_per_sec = ops.len() as f64 / elapsed.as_secs_f64();
    let stats = topology.stats(before_blob_l2_reads, before_page_hits);
    let allocations_per_op = measure_hit_allocations(candidate);
    let disqualified = !matches!(candidate, Candidate::BaselineShipped) && allocations_per_op > 0.0;
    MetricRow {
        workload: workload.name(),
        candidate: candidate.name(),
        p50_ns,
        p99_ns,
        ops_per_sec,
        allocations_per_op,
        stats,
        disqualified,
    }
}

fn apply_op(topology: &mut Topology, op: Op) {
    match op {
        Op::Read(idx) => {
            black_box(topology.read(idx));
        }
        Op::Write(idx) => topology.write(idx),
    }
}

fn measure_hit_allocations(candidate: Candidate) -> f64 {
    let mut topology = Topology::new(candidate);
    for _ in 0..16 {
        black_box(topology.read(1));
    }
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Relaxed);
    for _ in 0..ALLOCATION_PROBE_OPS {
        black_box(topology.read(1));
    }
    COUNT_ALLOCATIONS.store(false, Ordering::Relaxed);
    ALLOCATION_COUNT.load(Ordering::Relaxed) as f64 / ALLOCATION_PROBE_OPS as f64
}

fn ops_for(workload: Workload, count: usize) -> Vec<Op> {
    match workload {
        Workload::PointReadHotL1 => (0..count)
            .map(|idx| Op::Read(1 + idx % (L1_SLOTS - 1)))
            .collect(),
        Workload::PointReadZipfianL2 => zipfian_keys(count, WORKING_SET)
            .into_iter()
            .map(Op::Read)
            .collect(),
        Workload::ColdScan => (0..count)
            .map(|idx| Op::Read((L1_SLOTS + idx) % WORKING_SET))
            .collect(),
        Workload::MixedWriteHeavy => (0..count)
            .map(|idx| {
                if idx % 10 < 6 {
                    Op::Write(WORKING_SET + idx % WORKING_SET)
                } else {
                    Op::Read(zipf_key(idx, WORKING_SET))
                }
            })
            .collect(),
    }
}

fn zipfian_keys(count: usize, len: usize) -> Vec<usize> {
    (0..count).map(|idx| zipf_key(idx, len)).collect()
}

fn zipf_key(idx: usize, len: usize) -> usize {
    let mut hash = (idx as u64)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    hash ^= hash >> 33;
    let unit = (hash as f64) / (u64::MAX as f64);
    let skewed = unit * unit * unit;
    ((skewed * len as f64) as usize).min(len - 1)
}

fn percentile(sorted: &[u128], pct: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) * pct) / 100;
    sorted[idx]
}

fn payload() -> Vec<u8> {
    (0..SLOT_BYTES).map(|idx| (idx & 0xff) as u8).collect()
}

fn payload_for(seed: usize) -> Vec<u8> {
    (0..SLOT_BYTES)
        .map(|idx| ((idx ^ seed) & 0xff) as u8)
        .collect()
}

fn payload_array(seed: u64) -> [u8; SLOT_BYTES] {
    let mut out = [0; SLOT_BYTES];
    for (idx, byte) in out.iter_mut().enumerate() {
        *byte = ((idx as u64 ^ seed) & 0xff) as u8;
    }
    out
}

fn assert_boundary_crossing(rows: &[MetricRow]) {
    for candidate in Candidate::all() {
        let candidate_rows: Vec<_> = rows
            .iter()
            .filter(|row| row.candidate == candidate.name())
            .collect();
        let l1_hits = candidate_rows
            .iter()
            .map(|row| row.stats.l1_hits)
            .sum::<u64>();
        let l2_hits = candidate_rows
            .iter()
            .map(|row| row.stats.l2_hits)
            .sum::<u64>();
        assert!(
            l1_hits > 0,
            "{} did not record any L1 hits across the matrix",
            candidate.name()
        );
        assert!(
            l2_hits > 0,
            "{} did not record any L2 hits across the matrix",
            candidate.name()
        );
    }
    for row in rows {
        if row.workload == Workload::PointReadHotL1.name() {
            assert!(
                row.stats.l1_hits > 0,
                "{} {} did not record L1 hits",
                row.workload,
                row.candidate
            );
        }
    }
}

fn print_rows(rows: &[MetricRow]) {
    eprintln!("\n[cache topology spike #1970]");
    eprintln!(
        "workload,candidate,p50_ns,p99_ns,ops_per_sec,allocations_per_op,l1_hits,l2_hits,misses,evictions,writes,disqualified"
    );
    for row in rows {
        eprintln!(
            "{},{},{},{},{:.0},{:.3},{},{},{},{},{},{}",
            row.workload,
            row.candidate,
            row.p50_ns,
            row.p99_ns,
            row.ops_per_sec,
            row.allocations_per_op,
            row.stats.l1_hits,
            row.stats.l2_hits,
            row.stats.misses,
            row.stats.evictions,
            row.stats.writes,
            row.disqualified
        );
    }
}

fn maybe_write_report(rows: &[MetricRow]) {
    let Ok(path) = std::env::var("REDDB_CACHE_TOPOLOGY_REPORT") else {
        return;
    };
    let commit =
        std::env::var("REDDB_CACHE_TOPOLOGY_COMMIT").unwrap_or_else(|_| "unknown".to_string());
    let report = render_report(rows, &commit);
    let path = report_path(&path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create cache topology report directory");
    }
    std::fs::write(path, report).expect("write cache topology report");
}

fn report_path(path: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(path);
    if path.is_absolute() {
        return path;
    }
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .expect("server crate lives under crates/reddb-server");
    repo_root.join(path)
}

fn render_report(rows: &[MetricRow], commit: &str) -> String {
    let mut out = String::new();
    out.push_str("# Cache Topology Spike, Issue #1970\n\n");
    out.push_str(&format!("Measured commit: `{commit}`\n\n"));
    out.push_str(
        "Command: `REDDB_CACHE_TOPOLOGY_COMMIT=$(git rev-parse --short HEAD) REDDB_CACHE_TOPOLOGY_REPORT=docs/perf/cache-topology-spike-2026-07-10.md cargo bench -p reddb-io-server --bench cache_topology_spike_bench -- --nocapture`\n\n",
    );
    out.push_str("Criterion lane rule: compare rows only within this run. The report is not a cross-run delta.\n\n");
    out.push_str("## Hypotheses\n\n");
    for candidate in Candidate::all() {
        out.push_str(&format!(
            "- `{}`: {}\n",
            candidate.name(),
            candidate.hypothesis()
        ));
    }
    out.push_str("\n## Results\n\n");
    for workload in Workload::all() {
        out.push_str(&format!("### {}\n\n", workload.name()));
        out.push_str("| candidate | p50 ns | p99 ns | ops/s | allocations/op | L1 hits | L2 hits | misses | evictions | disqualified |\n");
        out.push_str("|:--|--:|--:|--:|--:|--:|--:|--:|--:|:--|\n");
        for row in rows.iter().filter(|row| row.workload == workload.name()) {
            out.push_str(&format!(
                "| `{}` | {} | {} | {:.0} | {:.3} | {} | {} | {} | {} | {} |\n",
                row.candidate,
                row.p50_ns,
                row.p99_ns,
                row.ops_per_sec,
                row.allocations_per_op,
                row.stats.l1_hits,
                row.stats.l2_hits,
                row.stats.misses,
                row.stats.evictions,
                if row.candidate == Candidate::BaselineShipped.name() {
                    "control"
                } else if row.disqualified {
                    "yes"
                } else {
                    "no"
                }
            ));
        }
        out.push('\n');
    }
    out.push_str("## Verdicts\n\n");
    for candidate in Candidate::all() {
        let candidate_rows: Vec<_> = rows
            .iter()
            .filter(|row| row.candidate == candidate.name())
            .collect();
        let avg_ops = candidate_rows
            .iter()
            .map(|row| row.ops_per_sec)
            .sum::<f64>()
            / candidate_rows.len() as f64;
        let max_allocations = candidate_rows
            .iter()
            .map(|row| row.allocations_per_op)
            .fold(0.0, f64::max);
        if matches!(candidate, Candidate::BaselineShipped) {
            out.push_str(&format!(
                "- `{}`: control row for the shipped topology. Average throughput {:.0} ops/s; measured hit-path allocations/op max {:.3}.\n",
                candidate.name(),
                avg_ops,
                max_allocations
            ));
        } else {
            let disqualified = candidate_rows.iter().any(|row| row.disqualified);
            out.push_str(&format!(
                "- `{}`: {} Average throughput {:.0} ops/s; hit-path allocations/op max {:.3}.\n",
                candidate.name(),
                if disqualified {
                    "rejected by allocation invariant."
                } else {
                    "passes the allocation invariant."
                },
                avg_ops,
                max_allocations
            ));
        }
    }
    let recommendation = rows
        .iter()
        .filter(|row| row.candidate != Candidate::BaselineShipped.name())
        .fold(
            HashMap::<&str, (f64, usize, bool)>::new(),
            |mut acc, row| {
                let entry = acc.entry(row.candidate).or_insert((0.0, 0, false));
                entry.0 += row.ops_per_sec;
                entry.1 += 1;
                entry.2 |= row.disqualified;
                acc
            },
        )
        .into_iter()
        .filter(|(_, (_, _, disqualified))| !*disqualified)
        .filter(|(_, (_, count, _))| *count > 0)
        .fold(None::<(&str, f64)>, |best, row| {
            let current = (row.0, row.1 .0 / row.1 .1 as f64);
            match best {
                Some((_, best_ops)) if best_ops >= current.1 => best,
                _ => Some(current),
            }
        })
        .map(|(candidate, _)| candidate)
        .unwrap_or(Candidate::BaselineShipped.name());
    out.push_str(&format!(
        "\nRecommendation: adopt `{recommendation}` as the follow-up implementation candidate. It stays at 0.000 hit-path allocations/op in this run and posts the best non-disqualified throughput row, while the shipped baseline remains the control for production until that follow-up lands behind normal correctness and compatibility gates.\n"
    ));
    out
}

criterion_group!(benches, cache_topology_spike);
criterion_main!(benches);
