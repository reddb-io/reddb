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

## CI Signal

`.github/workflows/dst-nightly.yml` runs this lane as the `Storage fault
recovery` job. The job uploads `dst-storage-fault-report` with the captured test
log on every run. On failure it opens a `release-blocker` issue with the
workflow link and reproduction checklist.

This is intentionally narrower than a full deterministic cluster simulator. It
keeps the first gate cheap and reproducible while covering the storage
invariants most likely to be violated by power cuts or broken filesystem
behavior.
