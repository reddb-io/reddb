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
  dropped or reordered fsync, ENOSPC, partial rename, and seeded power cuts.
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
