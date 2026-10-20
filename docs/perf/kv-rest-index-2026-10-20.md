# REST KV key-index integration — 2026-10-20

REST KV GET now uses the runtime's indexed key resolver instead of cloning the
entire collection. The application port keeps its existing read-contract check
and projects only the value and physical entity ID. Absent `value` fields still
produce JSON null; absent keys still return HTTP 404. Compact rows now use the
same field accessor as named rows. Versioned reads select a snapshot-visible
version rather than returning the first physical history row.

At 10,000 keys and one connection, the fresh before/after capture changed from
84 to 1,782 GETs/s (21.2x observed throughput), with median round-level p99 from
18.011 to 0.950 ms. This is an observed local diagnostic difference, not an
independent-run causal estimate or a competitive-lead claim. Concurrent small
datasets and durable writes did not show a consistent improvement.

The resolver is shared with `KvAtomicOps` and RQL. Each entry point retains its
own model-validation rules, including REST's permissive legacy implicit/dynamic
contracts. HTTP authentication and request identity middleware are unchanged.
Projection runs under a segment read guard and does not re-enter storage.

Cost sketch: N cloned collection rows per request become expected O(S + V) key
candidates for S segments and V retained versions of the requested key. This
does not bound S or make the complete HTTP request allocation-free. The index's
documented fallback still scans segments invalidated by raw mutable access.

The same port lookup is used to locate the previous row for INCR/CAS and direct
port deletion. Ordinary REST PUT already uses `KvAtomicOps::set` and its indexed
delete path, so replacing this port's read scan should not be assumed to solve
the durable-overwrite plateau. No WAL, fsync, write ordering, admission-cap or
file-format changes are included.

## Experiment

This is a sequential, shared-host before/after diagnostic of RedDB product paths,
using the [existing benchmark contract](../../bench/kv/COMPARE.md). It is not a
new competitive comparison or a maximum-throughput claim. Both binaries include
the earlier value-projection change; neither is an unmodified release binary.

- Before: `a3af4d6261f05fcc2dccd5caca178c5ddf6936a3`, binary SHA256
  `00e800ab0494a8b24fb9e58a1f23f16e3e692ad2fdac0a288cb7b4e9895ac3e1`.
- After: `5ad0766eda49f7472bf85ee91524a17b3ec10673`, binary SHA256
  `2ac44ed0bd18f5bb8c3244724cd9a6825dd0bda712896dd201c8fafc1c7653e2`.
  Subsequent test-fixture corrections do not change runtime code.
- Optimized Linux build: Rust 1.95.0, `cargo build --release --locked --bin red -j 4`.
- Each server: four CPUs (0–3), 2 GiB container limit, 1 GiB engine budget,
  `strict` durability, four workers, `--http-max-handlers 64`, result cache off.
- Each client: two CPUs (4–5), 1 GiB, persistent HTTP/1.1 connections. Separate
  fresh data volumes; before runs first, then after. Host activity remains uncontrolled.
- GET: 1,000 and 10,000 keys, 100-byte deterministic values, 1/8/32 connections,
  five two-second rounds per cell, every returned value checked. RQL is retained
  as a control path. HTTP pipeline 16 remains unsupported by the harness.
- PUT: 1,000 keys, 1/8 connections, five rounds of 500 overwrites with changing
  payloads and readback. Full state is verified after graceful reopen and after
  SIGKILL. Process-crash checks do not prove power-loss recovery.

Raw artifacts and container/build identities are retained separately for
[before](../../bench/results/kv-compare-rest-before-2026-10-20/) and
[after](../../bench/results/kv-compare-rest-index-2026-10-20/). Five rounds reuse
one server process; their medians and ranges are descriptive, not independent
run-level confidence intervals. Fixed run order and the Python client limit
causal and maximum-capacity conclusions.

## Results

All figures below are medians of five round-level statistics. The primary GET
metric is p99 request completion latency; ranges and every cell remain in the
two raw summaries.

| Keys | Connections | REST GET/s before | After | p99 ms before | After |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 | 1 | 858.8 | 2,025.5 | 1.861 | 0.765 |
| 1,000 | 8 | 2,755.4 | 2,911.0 | 7.118 | 9.751 |
| 1,000 | 32 | 3,147.2 | 2,617.5 | 30.166 | 46.401 |
| 10,000 | 1 | 84.2 | 1,782.0 | 18.011 | 0.950 |
| 10,000 | 8 | 239.9 | 2,212.8 | 65.005 | 23.859 |
| 10,000 | 32 | 156.7 | 2,404.8 | 343.245 | 61.067 |

The collection-size penalty is substantially smaller in the serial capture:
going from 1,000 to 10,000 keys reduced observed throughput by about 90% before
and 12% after. The source removes full-collection materialization from this
path; these two sizes do not prove constant-time end-to-end requests.

The 1,000-key concurrent tails worsened, and at 32 connections throughput fell
17%. These cells remain unfavorable. The unchanged RQL control path moved
between -31% and +43% throughput across its six cells, so fixed-order shared-host
variation and the client must be separated from engine effects before treating
any percentage as a stable regression or speedup. At 10,000 keys/32 connections,
sampled server CPU changed from roughly 405% to 79%, while client CPU changed
from 11% to 125% (100% per core). Client cost is now prominent; the measured
throughput does not establish server capacity. Small-dataset concurrent latency
still needs an alternating, independently repeated measurement and profiling.

| Connections | Strict REST PUT/s before | After | p99 ms before | After |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 99.8 | 76.7 | 16.653 | 45.941 |
| 8 | 98.3 | 102.0 | 182.310 | 122.355 |

There is no consistent durable-write gain. Eight-connection throughput remains
near 100 writes/s, and the serial cell regressed in this capture. Trace mutation
serialization, delete/create work and persistence waits separately; the GET
optimization does not resolve or attribute that plateau.

## Verification

Both matrices completed with `valid-diagnostic` status: 28 timed measurement
cells, 24 unsupported HTTP-pipeline cells retained, no invalid cells, and eight
successful SIGKILL/reopen/full-state checks. Graceful reopens also passed.
All measured responses and durable overwrite generations were verified.
Each raw summary includes 38 SHA256 checksums, including its manifest.

Five new REST regression tests cover response/collection isolation, CRUD and
null/missing keys, compact sealed rows, legacy versus declared model contracts,
latest versions/tombstones and snapshot filtering across sealed versions.
The 27 existing KV unit tests passed, including the 8,000-increment concurrent
counter test and persistent reopen. Local `cargo check --locked`, the optimized
Linux build and all 12 benchmark-harness tests passed.

[CI on runtime/test revision `3b726072`](https://github.com/reddb-io/reddb/actions/runs/39469886698)
passed formatting, workspace check/Clippy, all four test shards, RQL conformance
and driver checks. Later changes add documentation and captured evidence.

```sh
cargo test -p reddb-io-server --lib rest_kv_index -- --nocapture
cargo test -p reddb-io-server --lib runtime::impl_kv::tests -- --nocapture
cargo check --locked
python -m unittest discover -s bench/kv -p test_compare.py -v
```
