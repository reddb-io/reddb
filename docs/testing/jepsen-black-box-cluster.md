# Jepsen-Style Black-Box Cluster Harness

`scripts/jepsen_black_box_cluster.py` is the free local outer-loop harness for
RedDB process-level distributed safety checks. It starts real `red` binaries,
drives public HTTP queries, injects externally visible faults, and records a
replayable history when a checker fails.

## Running

Build `red`, then run the harness:

```bash
cargo build --bin red
python3 scripts/jepsen_black_box_cluster.py --seed 0x5eed --red-bin target/debug/red --keep-artifacts
```

The harness creates one primary and two replicas with private data paths under
`target/jepsen-blackbox/`. Replicas connect to the primary through local TCP
proxies owned by the harness. Client operations use the HTTP `/query` and
`/health` surfaces, not in-process Rust APIs.

The fast contract test does not boot servers:

```bash
cargo test --test grouped_replication jepsen_black_box_harness_self_test_exercises_replay_artifacts_and_checkers
```

## Faults Covered Locally

- Process kill and restart of a real `red` process, preserving the node data
  path across restart.
- Message isolation between a replica and the primary by closing and rejecting
  traffic in the replica's local TCP proxy.
- Continued client writes and reads through public HTTP while faults are active.
- Replica write attempts during isolation, which must fail closed rather than
  accepting writes as a stale or secondary writer.

The proxy-based isolation is intentionally local and free: it needs no root
firewall rules, Docker daemon, paid provider, or kernel extension.

## Checkers

Each run writes `history.jsonl` as operation events and `schedule.json` as the
fault schedule. The checkers assert:

- no acknowledged committed write is absent from the final primary read after
  recovery;
- no non-primary node accepts a write while fenced as a replica or isolated
  stale writer;
- no safety window records successful writes from more than one writer.

On failure, the run directory is preserved with `repro.json`, `history.jsonl`,
`schedule.json`, and per-node logs. `repro.json` includes the seed and a rerun
command.

## Boundary Versus Deterministic Hypervisor Testing

This harness is a black-box process harness. It covers real `red` startup,
public transports, process death, restart, and local message isolation. It is
not deterministic hypervisor replay:

- no packet-level scheduler for every host interface;
- no deterministic VM clock or CPU scheduling replay;
- no whole-machine power cut or block-device fault timeline;
- no automatic minimization of a failing interleaving.

Use it with the model-level Maelstrom-style test and the `unreliable-libc` DST
storage fault stack. Together they give free coverage for protocol safety, real
process behavior, and durability faults, while leaving deterministic hypervisor
replay as an explicitly uncovered provider capability.
