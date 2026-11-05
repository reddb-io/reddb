# Architectural follow-up evidence

This is a checked shared-host diagnostic, not a KiviDB comparison or a capacity
benchmark. See [the report](../../../docs/perf/rql-sql-projection-2026-10-20.md)
for interpretation, unfavorable cells and remaining costs.

| Capture | Baseline | Candidate | Purpose |
| --- | --- | --- | --- |
| Root matrix | main `0eb3a89c` | `90feffe7` | Exploratory full ABBA read matrix |
| `final/` | main `0eb3a89c` | `2584e484` | Closed read matrix after numeric hash-miss fallback repair |
| `writes/main-*` | `0eb3a89c` | — | Strict/grouped baseline syscall profiles |
| `writes/pr-*` | — | `2584e484` | Strict/grouped candidate syscall profiles |
| `writes-final/` | — | `fae39eb6` | Repeat candidate write profiles after session failure fencing |

Every read matrix has 32 valid cells, checked values/order and full SQL/KV
verification after process termination and reopening. Each write profile checks
64 overwrites per phase and persisted results after process termination. Raw
syscall durations include tracing overhead and cannot establish throughput.
Container image identities, commands, limits and environment are in the capture
manifests/profiles. The Python client and server have separate pinned CPU sets.

The `final` read results describe `2584e484`. The later `fae39eb6` changes session
failure fencing, Strict coordinator participation and backend eligibility; its
write profiles are separate. Read timings have not been relabeled as that source.
The early exploratory matrix is not pooled with `final`.

Complete Git archives supplied each trusted Linux source tree. Every archived
file was compared with the build tree; changed files were copied without
preserving old modification times. The canonical main binary is
`architecture-main-0eb3a89c-red`, SHA256
`6afcf03b3cc7aa42d693f02bdf653f7ca1cda2b9e3e125940b8e4551c8626c56`.
The read candidate is `architecture-pr-2584e484-red`, SHA256
`9b015e9f6d51cd8a3e93ff2af75fa2487cfc7ce4da9a94df842b30562a20d0dd`.
The immutable final write binary's hash is recorded in each `writes-final`
profile. Build logs are retained alongside these artifacts.

Two earlier build attempts were discarded: an overlay over stale cached source,
and a baseline build that reused an executable because preserved source mtimes
misled Cargo. Neither binary supplied the canonical captures above.
`invalidated-linux-overlay.log` records the mixed-source test attempt and is
not a test failure of the current revision. `sql-point-before-cpu.txt` profiles
older revision `8eb7f554`
only to diagnose cloning; it is not the main timing baseline.

The broad native KV test run had 46 passes and one Windows paged/Vault opening
failure with OS error 33. `windows-vault-baseline.txt` reproduces that failure
on clean main; `windows-vault-current.txt` records the corresponding candidate
failure. This limitation is unresolved and separate from the optimized
single-file runtime path. Linux CI validates the paged Strict flush regression.

`run-write-profiles.py` preserves the original four write cases.
`run-final-profiles.py` creates fresh final-revision containers and volumes.
Both require the local build volume and network described in their arguments.
Do not run them into existing output directories or overwrite closed captures.
Text is normalized to LF before summaries and the evidence checksum manifest
are generated. CI verifies those committed bytes.
