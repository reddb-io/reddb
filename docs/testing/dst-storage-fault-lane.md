# DST Storage Fault Recovery Lane

The storage fault lane promotes the existing `unreliable-libc` crash-recovery
suites into an explicit local and nightly signal. It is the first RedDB maturity
step toward Turso-style deterministic simulation testing for durability bugs:
exercise seeded storage faults now, then grow the simulator surface over time.

## Running Locally

```bash
make test-dst-storage
```

To reproduce a specific failing interleaving from CI:

```bash
SEED=<n> make test-dst-storage
```

The lane currently runs these suites:

- `sim_power_cut_recovery`: in-process `SimVfs` coverage for torn writes,
  dropped or reordered fsync, ENOSPC, partial rename, seeded power cuts, and the
  four named ADR 0074 §1 fault classes (see [Fault Classes](#fault-classes)).
- `value_equivalence_recovery`: typed-value crash recovery, asserting recovered
  committed values match the model's committed prefix.
- `power_cut_recovery`: Linux `LD_PRELOAD` shim coverage for real process kills,
  short writes, and injected I/O failures around the WAL workload.
- `tm_commit_path_recovery`: TM v2 commit-path campaign for the four #1651
  scenario families:
  `fcw_before_wal_append`, `wal_append_before_finalize`,
  `savepoint_release_rollback`, and `concurrent_writers`.
- `store_fork_lifecycle_recovery`: store-fork lifecycle campaign for #1784,
  injecting power cuts during fork creation, first-touch hydration, and
  promotion.

## Fault Classes

ADR 0074 §1 names the storage faults RedDB models. Four of them are injectable
knobs; crash-at-any-point is modeled separately by the existing `buggify!` crash
boundaries and the `power_cut_after` budget.

Each class is selected **by name** with its own probability in parts-per-million,
is **off by default** (`0` ppm), and composes freely with the crash and delay
knobs. Every decision is drawn from the campaign's seeded stream, so the same
seed reproduces the same fault schedule byte for byte. In release builds the
`buggify_fault!` macro compiles to `None`, exactly like `buggify!` — no
production write path can be perturbed.

| Knob | Semantics |
| --- | --- |
| `torn_write` | The write persists only a prefix of its buffer, cut at a simulated 512-byte sector boundary, then reports the full length. A write that never crosses a sector — the common case for small WAL frames — is cut at a strict byte prefix instead (sub-sector tearing). Set `torn_write_crashes` to cut the power after the tear rather than report success. |
| `misdirected_write` | The write persists the correct bytes at a wrong offset — a seed-derived whole number of sectors before or after the requested one — and reports success. |
| `bit_rot` | Stored bytes come back with one flipped bit on a later read. Rolled on the **read** side (`VfsFile::read` and `SimVfs::crash_image`, the recovery reader's read), so the write path is untouched and the device keeps what was written. |
| `lost_write` | The write, or its fsync effect, is dropped entirely while success is reported. |

Arm them through `SimFaultConfig`, or through the `SimulationContext` when a
campaign drives its decisions off `buggify!`:

```rust
let cfg = SimFaultConfig::none()
    .with_fault_class(FaultClass::TornWrite, 20_000)   // 2% per write
    .with_fault_class(FaultClass::BitRot, 150_000);    // 15% per file read back

let context = SimulationContext::new(seed)
    .with_fault_class(FaultClass::LostWrite, 15_000);
```

An installed `SimulationContext` takes over the coin flips and their
probabilities, so a `buggify!`-driven campaign stays on the seed's single
decision stream. `bit_rot` wants a higher ppm than the write-side classes to
appear as often across a sweep: it is rolled once per file when the recovery
reader reads the crash image, not once per write.

### Fault Log

Every *applied* injection is appended to a machine-readable fault log — reachable
as `SimVfs::fault_log()` (device-scoped) and `SimulationGuard::fault_log()`
(campaign-scoped). When several classes fire on one write only the applied one is
recorded, so the log names exactly the durable bytes that were touched. An oracle
consumes the `FaultRecord` structs directly; `fault_log_lines()` renders the same
data as `key=value` lines:

```text
class=torn_write file=/db/wal.log offset=1088 length=48 persisted=12
class=misdirected_write file=/db/wal.log offset=4096 length=64 actual_offset=3584
class=bit_rot file=/db/wal.log offset=0 length=2048 byte_offset=917 bit=3
class=lost_write file=/db/super.block offset=64 length=64
```

The `sim_power_cut_recovery` campaign uses this to hold two contracts: a recovery
invariant is never violated without an injected fault to explain it (a crash
alone must always recover cleanly), and a checksum-detectable injection — bit rot
or a torn write on a checksummed region — surfaces as a **detection event** (an
oracle error, or a truncated recovery with a non-zero torn tail) rather than as a
silently-accepted commit frontier.

Scrub and salvage (ADR 0074 §3–4) build on this vocabulary; they are later
slices.

## Documented Seeds

The TM commit-path campaign sweeps seeds `0..15` by default. `SEED=<n>` pins a
single reproduction seed across all four scenario families:

```bash
SEED=7 cargo test --locked -p unreliable-libc --test tm_commit_path_recovery
```

Each scenario runs as a seeded workload, injects a crash through deterministic
WAL truncation, the in-process `SimVfs` power-cut path, and on Linux the
`LD_PRELOAD` power-cut shim. Restart first runs the shared WAL recovery oracle,
then asserts the TM-specific invariant: single-transaction scenarios recover as
fully absent or fully committed, the concurrent-writer scenario recovers only a
transaction prefix, and savepoint sub-xids are never orphan-visible.

The store-fork lifecycle campaign prepares the operation-specific parent/fork
state outside the shim, then runs exactly one lifecycle operation under
`LD_PRELOAD` while enumerating deterministic kill points. Recovery runs the
shared WAL oracle and the operational-manifest checks so interrupted create,
hydrate, and promote stages leave either the pre-crash state or the completed
state, never a manifest that silently points at a missing or half-hydrated
artifact.

## CI Signal

`.github/workflows/dst-nightly.yml` runs this lane as the `Storage fault
recovery` job. The job uploads `dst-storage-fault-report` with the captured test
log on every run. On failure it opens a `release-blocker` issue with the
workflow link and reproduction checklist.

This is intentionally narrower than a full deterministic cluster simulator. It
keeps the first gate cheap and reproducible while covering the storage
invariants most likely to be violated by power cuts or broken filesystem
behavior.
