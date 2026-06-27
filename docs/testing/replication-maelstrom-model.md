# Replication Maelstrom-Style Protocol Model

`tests/grouped/replication/maelstrom_protocol_model.rs` is a fast executable
model for the replication/election safety rules captured in `specs/tla/`. It
does not start RedDB servers and does not require Maelstrom, Jepsen, Docker, or
paid external infrastructure. The test uses an in-process scheduler to apply
Maelstrom-compatible faults to a three-node model:

- message loss by dropping queued protocol messages;
- message delay by moving queued messages into later scheduler steps;
- message reorder by swapping queued message delivery order;
- network partitions by cutting links between one node and the rest;
- modeled process crashes and restarts while durable terms, votes, and logs
  remain on the node.

Each run is deterministic from its seed. A safety failure reports a trace id in
the form `seed=0x...`, the failing step, the violated property, and the scheduled
actions that led there. Re-running the same seed reproduces the same schedule.

## Properties checked

The harness asserts the same narrow safety envelope as the formal model:

- **single-writer/election safety**: at most one leader exists in a term;
- **no stale-leader writes**: a leader cannot accept a modeled client write after
  a higher elected term is known;
- **no committed-write loss**: once a log entry is committed at an index, later
  leaders must carry the same entry at that index.

This is a protocol model, not a storage-engine or wire-compatibility test. It
models leader election, vote grants, append/commit behavior, crash/restart, and
faulty message delivery. It intentionally does not model:

- real RedDB process startup, gRPC/HTTP/RedWire transport, TLS, or auth;
- physical WAL files, logical replication spool encoding, basebackup, or disk
  corruption;
- operator workflows, multi-process deployment, clock behavior, or live cluster
  observability;
- arbitrary client workloads beyond a small write/commit protocol event.

## Boundary with Jepsen-style cluster testing

The later black-box cluster harness should run real `red` processes and validate
user-visible behavior through public transports while faulting the environment.
That harness belongs at the Jepsen-style layer: process kill, network partitions,
disk faults, restart/rejoin, client histories, and linearizability or durability
checking against observed database results.

This model is earlier and cheaper. It turns the TLA+ invariants into randomized
distributed executions so protocol mistakes surface quickly with replayable
seeds before a full cluster harness exists.
