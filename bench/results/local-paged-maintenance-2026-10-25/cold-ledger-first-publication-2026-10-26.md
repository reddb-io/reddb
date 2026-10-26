# Cold first audit event: small reproduction

Question: does one real audit event before the first checkpoint after reopening
retain the incremental full-text path observed for the same warm insertion?

**Answer: no, reproduced on runtime `bae760f43`.** This is an incremental
admission/performance failure; content and acknowledged-write recovery passed.
No production fix or new release candidate was built from this experiment.

The frozen Rust test is adjacent to this report. Run it as an additional test
in `storage::unified::devx::reddb::native_source_tests` on an isolated worktree
of the runtime source. It uses real `RuntimeLedger`, `AuthLogin` events and the
existing test-only `APPLIED_ROWS` counter. It creates six events, checkpoints,
adds one warm event, checkpoints, closes, reopens and adds one further event
before its first checkpoint. It then checks exact persisted/full-derived text
frames and reopens again to verify all eight events, including the final
acknowledged event ID. It asserts one-row incremental admission in both paths.

| Observation | Result |
| --- | --- |
| Warm one-event applied row count | `Some(1)` |
| Cold first-event applied row count | `None` (full builder fallback) |
| Warm/cold text frame lengths | 1,447 / 1,650 bytes |
| Exact frame matches complete derivation | Pass, both stages |
| Recovery of eight events and final acknowledged ID | Pass |
| Cold opening | 84,284 microseconds |
| First publication | 51,863 microseconds |
| Opening plus first publication | 136,147 microseconds |
| One-row cold admission requirement | Fail |

The actual test completes in 0.57 seconds after a 4m17s test-library compile.
Compilation/testing used the accepted one-at-a-time 8 GiB RAM / 4 GiB swap
scope, with two CPU cores. The temporary injection into the source test module
was removed with exact before/after SHA-256 verification; the frozen failing
probe remains here as diagnostic evidence, not an ignored passing-CI test.

This confirms the mechanism proposed after RC378 rejection: in-memory native
source proofs start empty and cold proof recovery occurs only during checkpoint
preparation. A prior audit insertion changes the source digest, so the old proof
cannot be admitted and the safe fallback rebuilds the frame. It does not yet
attribute how much of the historical 9.629-second preparation cost is hashing,
retained-payload validation or construction, nor prove a bounded-memory fix.

The next change must preserve source identity, mutation visibility and durable
publication while avoiding needless reconstruction. Measure opening plus first
publication together. Moving the same scan to startup is not by itself reduced
work. Do not relax the historical 10-second/64 MiB acceptance gates. The separate
OOM observed during live staging catalog/RAM inspection requires its own isolated
reproduction before further live profiling; allocator-only mitigation was
insufficient and a temporary in-place 5 GiB limit is operational headroom only.
