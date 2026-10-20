# KV comparison diagnostics

This is separate from the historical `run.py` / `check-regression.py` contract.
It measures real HTTP/1.1 keep-alive paths for RedDB and RESP2 for Redis/KiviDB.
The transport and response envelopes differ: this is a product-path comparison,
not a storage-engine-only comparison or a Redis-protocol compatibility claim.

The preselected primary metric is **GET p99 completion latency at pipeline 1**.
Secondary metrics are p50/p95, wall-clock throughput, errors, sampled server/client
CPU and cgroup working-set memory, network bytes and block I/O. Pipeline 16 is a
separate RESP-only diagnostic. Its latency is completion of the entire batch,
including validation; it is never divided by 16 to invent per-request latency.

The initial matrix is 1,000 / 10,000 keys, deterministic 100-byte text values,
1 / 8 / 32 connections, ten warmup GETs per connection and five two-second GET
rounds. Every returned value is checked. Full dataset verification brackets each
matrix and also follows restart. GET throughput is actual completed operations
divided by wall time after a synchronized warmup barrier, not summed latencies.
The fixed-count durable-overwrite diagnostic has five rounds of 500 logical
writes with 1 / 8 connections. Each round changes values, checks every changed
key, and the final full expected state is recorded for recovery checks.

Persistence conditions:

- RedDB: persistent file, `REDDB_DURABILITY=strict` and
  `REDDB_DURABILITY_MODE=strict`; result caching explicitly disabled.
- Redis: `appendonly yes`, `appendfsync always`, automatic snapshots disabled.
- KiviDB 1.1.0: AOF enabled, with **SET + WAITAOF 1 0 5000** as one timed logical
  write. The harness requires `[1, 0]` from the fence. Native SET alone is not
  compared as sync-durable: `CONFIG SET appendfsync always` can return OK while
  leaving the periodic fsync behavior unchanged. See the pinned
  [config implementation](https://github.com/kividbio/kividb/blob/v1.1.0/src/config.rs)
  and [AOF writer](https://github.com/kividbio/kividb/blob/v1.1.0/src/aof.rs).

After each GET matrix and durable-write cell the runner sends SIGKILL, restarts
the same container/volume, then verifies all expected values. This tests process
crash recovery after completed work. It does not simulate power loss or kill a
writer midway through its request. Native write responses and the KiviDB
client-assisted fence are distinct product paths even though the harness waits
for a local persistence confirmation before counting either as a completed write.

The server gets CPU set 0–3, four CPUs, a 2 GiB process limit and a 1 GiB engine
memory budget. The client gets CPU set 4–5, two CPUs and 1 GiB. Engines run
sequentially; the client opens one persistent connection per worker. Python
threads, validation/serialization, the Docker VM and other host activity can
limit results. CPU and memory samples are Docker cgroup observations, not
per-key memory measurements. Missing samples are unavailable, never zero.

These runs are **shared-host diagnostics**. Five rounds share a server process
and are not independent host-level experiments. Raw round samples must remain
visible. They do not qualify for ADR 0076's public 20% leadership decision or a
confidence interval supporting that decision. Use exclusive hosts, independent
run-level repetitions and a proven non-saturating client for that stage.

## Reproduce

Requires Docker with at least six available CPU IDs and Python 3.10+. Use a clean
checkout of the recorded engine revision. The run manifest records image digests,
commands, environment, CPU/memory limits, engine revision and binary checksum.
The local harness has no Python package dependencies.

First build the optimized Linux binary. The initial capture used
`rust:1.95-slim-bookworm`, digest
`sha256:d7482085ff5b415f84dba5647ae71606650bdef00db7aeb69f4b3d170c3e4082`.
For a fresh run, use a distinct set of container/volume names and record them.
The following example uses the names consumed by `local_matrix.py`:

```sh
git archive --format=tar -o /tmp/reddb-kv-source.tar <engine-revision>
docker volume create reddb-kv-build-20261020
docker run --name reddb-kv-build-20261020 --cpus 6 --memory 10g \
  -v /tmp/reddb-kv-source.tar:/source.tar:ro \
  -v reddb-kv-build-20261020:/work \
  rust:1.95-slim-bookworm sh -c '
    set -eu
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends protobuf-compiler pkg-config libssl-dev ca-certificates
    mkdir -p /work/src
    tar -xf /source.tar -C /work/src
    cd /work/src
    cargo build --release --locked --bin red -j 4
    sha256sum target/release/red
  '
docker network create --internal reddb-kv-20261020
docker volume create reddb-kv-red-20261020
docker volume create reddb-kv-redis-20261020
docker volume create reddb-kv-kivi-20261020
docker create --name reddb-kv-red-20261020 --network reddb-kv-20261020 \
  --cpuset-cpus 0-3 --cpus 4 --memory 2g \
  -v reddb-kv-build-20261020:/build:ro -v reddb-kv-red-20261020:/data \
  -e REDDB_DURABILITY=strict -e REDDB_DURABILITY_MODE=strict \
  -e REDDB_MEMORY_BUDGET=1073741824 -e REDDB_RUNTIME_RESULT_CACHE_ENABLED=false \
  -e RUST_MIN_STACK=8388608 python:3.12-slim-bookworm \
  /build/src/target/release/red server --path /data/bench.rdb \
  --http-bind 0.0.0.0:5000 --workers 4 --no-auth
docker create --name reddb-kv-redis-20261020 --network reddb-kv-20261020 \
  --cpuset-cpus 0-3 --cpus 4 --memory 2g -v reddb-kv-redis-20261020:/data \
  redis:8.4.0-bookworm redis-server --save '' --appendonly yes --appendfsync always \
  --protected-mode no --io-threads 4 --maxmemory 1073741824 --maxmemory-policy noeviction
docker create --name reddb-kv-kivi-20261020 --network reddb-kv-20261020 \
  --cpuset-cpus 0-3 --cpus 4 --memory 2g -v reddb-kv-kivi-20261020:/home/kividb \
  kividb/kividb:v1.1.0 --aof --threads 4 --io-model tokio --protected-mode no \
  --update-check no --maxmemory 1073741824
mkdir -p bench/results/kv-compare-local
docker run -d --name reddb-kv-client-20261020 --network reddb-kv-20261020 \
  --cpuset-cpus 4-5 --cpus 2 --memory 1g \
  -v "$PWD/bench/kv:/bench:ro" -v "$PWD/bench/results/kv-compare-local:/results" \
  python:3.12-slim-bookworm sleep 36000
python bench/kv/local_matrix.py --out bench/results/kv-compare-local \
  --engine-revision <engine-revision> --engine-sha256 <sha256-from-build>
```

Use `--redis`, `--kividb`, `--reddb`, and `--client` to override container names.
Use `--keys`, `--workers`, `--rounds` and `--seconds` for additional diagnostic
matrices; keep each workload version and all unsuccessful cells in the results.
The runner stops its benchmark containers when finished and preserves their
volumes. It never publishes host ports or touches other running containers.

```sh
python -m unittest discover -s bench/kv -p test_compare.py -v
```

Tests reject protocol errors, closed/truncated responses, incorrect values,
missing fsync confirmations and acknowledged-but-discarded writes. They also
check uneven worker/batch quotas, fixed-duration execution and the real HTTP
query-result envelope. Invalid runs retain their diagnostic JSON and cannot
produce a successful aggregate throughput result.
