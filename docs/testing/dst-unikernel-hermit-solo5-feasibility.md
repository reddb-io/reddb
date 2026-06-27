# DST Unikernel Feasibility: Hermit and Solo5

Date: 2026-06-27
Issue: #1493
Parent: #1350

## Recommendation

Defer a Hermit/Solo5 prototype. Do not create an implementation issue yet.

The practical next work should stay on the existing free correctness stack:
the in-house DST path in `crates/unreliable-libc` and `crates/reddb-file`, a
Jepsen-style black-box harness for real `red` binaries, Maelstrom-style network
model exercises where the protocol can be adapted, and TLA+ for small
coordination contracts.

Revisit unikernels only after RedDB has a narrow, production-supported
storage/process abstraction that can run a single-node durability workload
without libc interposition, child processes, host filesystem paths, or normal
TCP listeners. Until then, a unikernel target would mostly test a port of RedDB,
not the production runtime.

## Source Boundary

This spike separates three similarly named ideas:

- Cash App Hermit is a hermetic tooling/package-manager project. Its docs say it
  installs isolated, self-bootstrapping project tools so developers and CI share
  consistent tooling. It is not a unikernel or fault-testing runtime.
- Hermit for Rust, from `hermit-os`, is the actual Rust unikernel project. Its
  Rust target is `*-unknown-hermit`, builds self-contained unikernel images, and
  is Tier 3 in Rust.
- Solo5 is not a Rust build tool. It is sandboxing middleware for
  unikernel-style applications, with tenders such as `hvt`, `spt`, and `virtio`.

## Upstream Facts

- Hermit for Rust describes itself as a Rust-based lightweight unikernel where
  the application is bundled directly with the kernel library and runs without
  an installed operating system:
  <https://github.com/hermit-os/hermit-rs>.
- Rust's official `*-unknown-hermit` target page marks Hermit targets as Tier 3,
  cross-compilation-only, `std`-capable, and not shipped with precompiled
  artifacts. It also says every Hermit program is a unikernel and that Rust does
  not support C code and Rust code together on these targets yet:
  <https://doc.rust-lang.org/nightly/rustc/platform-support/hermit.html>.
- The Hermit template builds with `cargo build --target x86_64-unknown-hermit`
  and runs the result under Uhyve or QEMU:
  <https://github.com/hermit-os/hermit-rs-template>.
- Hermit networking is feature/device-driven. The advanced configuration wiki
  shows TCP/UDP support through `smoltcp`, host TAP or user networking, and
  current network support via virtio-style devices. The same page documents
  VirtioFS as an optional QEMU-only filesystem-sharing path:
  <https://github.com/hermit-os/hermit-rs/wiki/Advanced-Configuration-Features>.
- Solo5's README calls Solo5 middleware between unikernel applications and host
  systems, not an end-developer product:
  <https://github.com/Solo5/solo5>.
- Solo5's building guide says supported targets include `hvt`, `spt`, and
  `virtio`; network tests require TAP setup; devices declared by an application
  manifest must be attached; `virtio` is a compatibility target and supports
  only a single network and/or block device:
  <https://raw.githubusercontent.com/Solo5/solo5/main/docs/building.md>.
- Solo5's architecture doc frames the host interface as intentionally thin,
  minimal, and unlike the Linux process or full QEMU machine interface:
  <https://raw.githubusercontent.com/Solo5/solo5/main/docs/architecture.md>.
- Maelstrom is a Jepsen workbench that runs plain binaries, routes JSON messages
  over a simulated network, injects latency/message loss/partitions, and checks
  histories:
  <https://github.com/jepsen-io/maelstrom>.
- Jepsen's public analysis page documents its production-oriented black-box
  database testing focus:
  <https://jepsen.io/analyses>.
- TLA+ and TLC remain the right fit for small coordination-state models; the
  TLA+ tools page identifies TLC as the main model checker:
  <https://github.com/tlaplus/tlaplus>.
- Antithesis documents packet-level fault injection and deterministic simulation
  as a hosted platform:
  <https://antithesis.com/docs/environment/fault_injection/>.
- Cash App Hermit documents itself as a hermetic package manager for per-project
  toolchains, which is why it must not be confused with Hermit OS:
  <https://cashapp.github.io/hermit/>.

## RedDB Fit Assessment

### Runtime

RedDB's `red` binary builds and owns normal Tokio runtimes with `enable_all()`.
The CLI also waits on `tokio::signal::ctrl_c()` for UI mode. Hermit can run some
Rust `std` applications, but the official target remains Tier 3, does not ship
precompiled artifacts, and cannot currently mix C and Rust code for the target.
That is already a poor fit for a workspace with generated/protobuf/wire/server
surfaces and existing test tooling that expects normal host processes.

### Filesystem and Durability

The current DST work is explicitly about real persistence behavior. The
workspace already contains `crates/unreliable-libc`, described in its manifest as
an LD_PRELOAD libc fault-injection shim plus WAL recovery oracle. Its crate docs
route real `write`, `pwrite`, `fsync`, `fdatasync`, and `rename` failure modes
through deterministic seeds. `crates/reddb-file::dst` adds in-process
`buggify!()` hooks with byte-identical traces for deterministic crash points.

That is closer to RedDB's production durability contract than a unikernel
filesystem port. Hermit filesystem access would require QEMU/VirtioFS setup for
host sharing, and Solo5 exposes block-device style attachment rather than a
normal host filesystem. Either path would force a storage adaptation layer before
testing the same path users run.

### Networking

RedDB exposes normal host TCP surfaces: gRPC, HTTP/HTTPS, RedWire, TLS RedWire,
PostgreSQL wire, and replica-primary addresses. The CLI flag surface assumes
`host:port` listeners and local process clients.

Hermit networking requires explicit target dependencies/features and virtio/TAP
or QEMU user-network setup. Solo5 examples attach network devices through a
manifest and TAP interface. That can test a custom network appliance, but it is
not a drop-in harness for the production `red server` process model.

### Process Model

The existing tests and CLI use ordinary process behavior. The root Cargo file
includes grouped integration tests, and client tests spawn real `red` binaries.
The CLI includes process exits, child-process spawning for browser open, and
system-service oriented paths. Maelstrom and Jepsen-style tests also deliberately
operate on plain processes.

Unikernel execution would remove or replace those process boundaries. That loses
coverage for the exact surfaces most users and future cluster tests rely on.

## Coverage Compared With Existing Free Paths

| Path | Good coverage | Weak coverage | Verdict |
| --- | --- | --- | --- |
| Hermit unikernel | A tiny single-binary Rust workload; potential VM reboot/power-cut experiments | Production TCP/process behavior, host filesystem semantics, libc fault hooks, multi-process cluster tests, C/protobuf-adjacent build surfaces | Not worth a prototype now |
| Solo5 | Very thin sandbox, fast boot, block/net device experiments | Not an end-user Rust app platform; requires unikernel-native porting and device manifests; does not run Linux applications as-is | Reject for RedDB prototype now |
| Jepsen-style black-box harness | Real `red` binary, real TCP, process crashes/restarts, partitions, history checking | Less deterministic than full DST; slower failure minimization | Proceed |
| In-house DST stack | Seeded local durability faults, WAL oracle, deterministic trace reproduction, real storage invariants | Initially narrower than whole-cluster simulation | Proceed |
| Maelstrom-style harness | Free simulated networks, message loss/latency/partitions, strong history checkers | Requires protocol adaptation to JSON/STDIO or a proxy | Proceed selectively |
| TLA+ | Exhaustive checking for small coordination specs and lifecycle invariants | Not an implementation/runtime fault harness | Proceed for contracts |

## Revisit Criteria

Create a new unikernel prototype issue only if all of these prerequisites land:

1. A committed minimal RedDB durability workload that runs without LD_PRELOAD,
   child-process orchestration, and host path assumptions.
2. A storage abstraction narrow enough to bind to a block-device or in-memory
   fake without bypassing the production WAL/recovery logic under test.
3. A no-network or single-socket workload whose pass/fail oracle is already
   shared with the in-house DST stack.
4. A measured gap showing the Jepsen-style harness plus in-house DST cannot
   exercise the targeted fault class.

Until those prerequisites exist, the unikernel path is complexity before
coverage.
