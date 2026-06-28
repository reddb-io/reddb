# Chaos Test Lanes

RedDB's local chaos coverage is split by fault layer so failures stay easy to
reproduce.

## Fast Local Matrix

Run the normal local chaos bundle:

```bash
make test-chaos-all
```

That target runs:

- `grouped_chaos_drill_persistence`: the consolidated local chaos, drill, and
  persistence binary.
- `make test-chaos-replication`: the Jepsen-style harness self-test and checker
  replay contract.
- `make test-dst-storage`: seeded storage fault recovery through
  `unreliable-libc` and `SimVfs`.

For a narrower rerun, call the failing target directly:

```bash
make test-chaos
make test-chaos-drills
make test-chaos-replication
```

Storage faults also support seed reproduction:

```bash
SEED=<n> make test-dst-storage
```

## Nightly And Heavy Lanes

The ignored replication DST sweep is heavier and is wired to the nightly DST
workflow:

```bash
make test-dst-sweep
```

To exercise the scheduled workflow manually from GitHub:

```bash
gh workflow run dst-nightly.yml
```

The full CI chaos jobs, including the Floci S3 backend chaos lane, run only on
manual full CI dispatch because they need extra runtime and Docker services:

```bash
gh workflow run ci.yml -f full_ci=true
```

## Black-Box Process Harness

The Jepsen-style black-box harness can run as a self-test without booting RedDB:

```bash
make test-chaos-replication
```

To run the real process-level harness, build `red` first and keep artifacts for
replay:

```bash
cargo build --bin red
python3 scripts/jepsen_black_box_cluster.py --seed 0x5eed --red-bin target/debug/red --keep-artifacts
```

The harness records `history.jsonl`, `schedule.json`, per-node logs, and
`repro.json` under `target/jepsen-blackbox/` when artifacts are kept.
