# Cache Topology Spike, Issue #1970

Measured commit: `3b6fa1c31`

Command: `REDDB_CACHE_TOPOLOGY_COMMIT=$(git rev-parse --short HEAD) REDDB_CACHE_TOPOLOGY_REPORT=docs/perf/cache-topology-spike-2026-07-10.md cargo bench -p reddb-io-server --bench cache_topology_spike_bench -- --nocapture`

Criterion lane rule: compare rows only within this run. The report is not a cross-run delta.

## Hypotheses

- `baseline-shipped`: Separate shipped SIEVE page cache plus Blob L1/L2 should remain the control.
- `unified-slot-arena`: One fixed-slot arena should remove hit-path allocation and improve L1-heavy reads.
- `promote-on-second-hit`: Promoting only after a second L2 hit should reduce scan churn under eviction pressure.

## Results

### point-read-hot-l1

| candidate | p50 ns | p99 ns | ops/s | allocations/op | L1 hits | L2 hits | misses | evictions | disqualified |
|:--|--:|--:|--:|--:|--:|--:|--:|--:|:--|
| `baseline-shipped` | 191 | 7874 | 2036488 | 2.000 | 1952 | 48 | 0 | 176 | control |
| `unified-slot-arena` | 40 | 55 | 15967171 | 0.000 | 2000 | 0 | 0 | 0 | no |
| `promote-on-second-hit` | 40 | 55 | 15900653 | 0.000 | 2000 | 0 | 0 | 0 | no |

### point-read-zipfian-l2

| candidate | p50 ns | p99 ns | ops/s | allocations/op | L1 hits | L2 hits | misses | evictions | disqualified |
|:--|--:|--:|--:|--:|--:|--:|--:|--:|:--|
| `baseline-shipped` | 670 | 13680 | 274097 | 2.000 | 1054 | 817 | 129 | 945 | control |
| `unified-slot-arena` | 301 | 337 | 4493948 | 0.000 | 833 | 1167 | 0 | 1167 | no |
| `promote-on-second-hit` | 44 | 353 | 7787950 | 0.000 | 1039 | 961 | 0 | 448 | no |

### cold-scan

| candidate | p50 ns | p99 ns | ops/s | allocations/op | L1 hits | L2 hits | misses | evictions | disqualified |
|:--|--:|--:|--:|--:|--:|--:|--:|--:|:--|
| `baseline-shipped` | 8661 | 22069 | 142993 | 2.000 | 250 | 1410 | 340 | 1538 | control |
| `unified-slot-arena` | 278 | 315 | 3290827 | 0.000 | 0 | 2000 | 0 | 2000 | no |
| `promote-on-second-hit` | 38 | 302 | 6616689 | 0.000 | 384 | 1616 | 0 | 768 | no |

### mixed-write-heavy

| candidate | p50 ns | p99 ns | ops/s | allocations/op | L1 hits | L2 hits | misses | evictions | disqualified |
|:--|--:|--:|--:|--:|--:|--:|--:|--:|:--|
| `baseline-shipped` | 3344036 | 5752031 | 449 | 2.000 | 427 | 300 | 73 | 1496 | control |
| `unified-slot-arena` | 268 | 312 | 3723771 | 0.000 | 277 | 523 | 0 | 1723 | no |
| `promote-on-second-hit` | 266 | 313 | 4238240 | 0.000 | 211 | 589 | 0 | 1487 | no |

## Verdicts

- `baseline-shipped`: control row for the shipped topology. Average throughput 613507 ops/s; measured hit-path allocations/op max 2.000.
- `unified-slot-arena`: passes the allocation invariant. Average throughput 6868929 ops/s; hit-path allocations/op max 0.000.
- `promote-on-second-hit`: passes the allocation invariant. Average throughput 8635883 ops/s; hit-path allocations/op max 0.000.

Recommendation: adopt `promote-on-second-hit` as the follow-up implementation candidate. It stays at 0.000 hit-path allocations/op in this run and posts the best non-disqualified throughput row, while the shipped baseline remains the control for production until that follow-up lands behind normal correctness and compatibility gates.
