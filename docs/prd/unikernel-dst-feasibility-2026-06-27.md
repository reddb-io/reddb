# Research: Unikernel DST outer-loop feasibility

GitHub: https://github.com/reddb-io/reddb/issues/1493
Date: 2026-06-27

## Recommendation

Defer the Hermit/Solo5-style unikernel outer loop. Do not create a RedDB
prototype issue yet.

Revisit only after RedDB has a narrow, host-independent storage harness that can
run the core WAL/pager/replication invariants without the production server
runtime, host filesystem contract, subprocess crash model, or `LD_PRELOAD`
fault shim. Until that prerequisite exists, the free correctness budget should
stay on the Jepsen-style black-box harness, Maelstrom-style control-plane tests,
TLA+ models, and the in-house DST stack.

## Scope

This spike evaluated whether a unikernel runtime could substitute for the paid
Antithesis hypervisor direction by giving RedDB a free outer loop for fault
injection. It did not evaluate unikernels as a deployment target, and it does
not propose production code changes.

## Tooling name collision

There are two different ideas with similar names:

- Hermit OS is an actual Rust unikernel/runtime project. It targets a single
  application image and provides the `*-unknown-hermit` Rust targets through a
  library operating system model.
- Cash App Hermit and other "hermetic build" tools are host-side package or
  toolchain isolation systems. They can make builds more reproducible, but they
  do not provide a guest kernel, virtual block device, network scheduler, power
  cut model, or RedDB process-fault surface.

This recommendation is about the first category only. Hermetic build tooling is
useful for CI reproducibility, but it is not a DST outer loop.

## Runtime fit

RedDB's current runtime shape is a poor match for a Hermit/Solo5-style target
without a compatibility rewrite:

| Surface | Current RedDB dependency | Unikernel fit |
|---------|--------------------------|---------------|
| Runtime | `reddb-io-server` depends on Tokio `rt-multi-thread`, Tonic, Hyper, Axum, `spawn_blocking`, background threads, and sync engine work behind async transports. | Hermit can run some Rust `std` applications, but validating the full async server stack would become a porting project. Solo5 is even smaller and normally expects a minimal guest ABI rather than a Linux-like process. |
| Filesystem | The storage path uses `std::fs`, `OpenOptions`, rename, directory creation, file metadata, file locks, `sync_all`, double-write buffers, WAL files, sidecars, manifests, and temp paths. | The value of RedDB's crash tests is tied to host filesystem semantics. Mapping that onto a unikernel block-device abstraction would test the shim as much as RedDB. |
| Networking | The production server binds `std::net::TcpListener`/`TcpStream`, bridges into Tokio listeners, and supports HTTP, gRPC, RedWire, WebSocket, TLS, and optional outbound HTTP clients. | A unikernel can expose network devices, but reproducing RedDB's public transports would require a specialized guest network harness. The current free black-box harness can exercise real host sockets directly. |
| Process model | Current DST work includes `crates/unreliable-libc`, a real `LD_PRELOAD` libc fault-injection shim, spawned workload binaries, `std::process::id`, deliberate crash exits, and host environment variables. | A unikernel collapses the process boundary. That removes the same host process/libc surface RedDB already uses for cheap crash and syscall fault injection. |
| Host inspection | Runtime/storage code reads Linux-ish state such as `/proc/meminfo`, `/proc/self/mountinfo`, and uses libc calls such as `fstatfs`/`ioctl` in the pager path. | These are not portable unikernel assumptions. Each use would need conditional behavior or a narrow harness that avoids the production path. |

The practical prototype path would therefore not be "run RedDB under a
unikernel." It would be "extract a reduced storage workload that avoids most of
the server and host-facing code." That is a useful idea, but it is not a free
substitute for Antithesis-style hypervisor faulting, and it should wait until
the host-independent harness prerequisite exists.

## Coverage comparison

| Approach | What it can cover well | What it misses for RedDB now | Cost/fit |
|----------|------------------------|-------------------------------|----------|
| Jepsen-style black-box harness | Real RedDB binaries, real sockets, process kill/restart, partitions, stale reads, linearizability-style histories, and operator-visible failure modes. | Less control over instruction-level scheduling and low-level disk faults unless paired with host fault shims. | Best free outer-loop fit today because it exercises production entry points. |
| Maelstrom-style control-plane tests | Deterministic message passing, partitions, reorder/loss, and replication/control-plane state machines without production I/O. | Does not validate the full storage engine or transport stack. | Good complement to RedDB's existing in-process replication DST. |
| In-house DST stack | Seeded `buggify!` boundaries, deterministic clocks/RNG traces, in-process message loss/reorder, and the `unreliable-libc` WAL recovery oracle. | Needs more coverage breadth and more invariants, but it already targets RedDB's actual seams. | Highest leverage for near-term free work because it is already in the repo and aligned with storage fault boundaries. |
| Hermit/Solo5 unikernel outer loop | Potentially repeatable single-image boot and device-level experiments once a narrow no-host harness exists. | Does not naturally cover RedDB's Linux process model, host filesystem semantics, `LD_PRELOAD` shim, subprocess crash model, or public server transports. | Defer. The porting/harness work would displace higher-value correctness work. |
| Antithesis-style hypervisor direction | Deterministic whole-system execution and richer external faulting without rewriting the application around a miniature guest ABI. | Paid dependency and separate operational integration. | Still the better hypervisor-shaped direction if RedDB decides it needs this class of outer loop. |

## Primary sources

- Hermit OS kernel repository: https://github.com/hermit-os/kernel
- Rust platform support for `x86_64-unknown-hermit`:
  https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-hermit.html
- Solo5 architecture documentation:
  https://github.com/Solo5/solo5/blob/master/docs/architecture.md
- Cash App Hermit package manager:
  https://github.com/cashapp/hermit
- Antithesis documentation:
  https://antithesis.com/docs/
- Jepsen project:
  https://github.com/jepsen-io/jepsen
- Maelstrom project:
  https://github.com/jepsen-io/maelstrom

## Decision

Reject a unikernel prototype for this cycle. The named prerequisite for
revisiting is a host-independent storage/correctness harness that can exercise a
small RedDB core without production server I/O, Linux process behavior, or
`LD_PRELOAD`. Until then, keep the next correctness slices on TLA+, Maelstrom,
Jepsen-style black-box testing, and the existing DST fault-injection stack.
