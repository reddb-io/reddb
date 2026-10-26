# Native artifact source provenance, 2026-10-26

The frozen `8e97eaaa346578b04353321dbb2f15f29de98e7d` runtime derives six
artifacts after a clean reopen, versus zero derivations and six retained
artifacts on warm checkpoints. Its WAL was eight bytes before and after the
graceful stop. Payload checks pass; this reproduces redundant construction,
not an independent demonstration of an unbounded storage leak.

Inspection of the first patch exposed an important distinction: runtime boot
writes current PID, start time and memory observations into `red_config`.
Its source really changes. The regression therefore separates an unchanged
engine reopen from a runtime reopen with that intentional write. The original
historical-store latency and physical-growth gates remain unchanged.

## Change

An optional `RASR` reference follows the existing artifact descriptors. It
points to an ordinary in-file native blob containing versioned `RASP` source
proofs. `reddb-file` owns both contracts. Each collection proof binds a BLAKE3
digest of its raw source to the exact published descriptors and summaries.
Cold admission validates the current source, descriptor set and every artifact
payload checksum before creating volatile manager/revision proofs. The source
includes raw transaction versions, so a checkpoint that omits unfinished rows
or normalizes their delete stamps cannot silently grant reuse on a different
recovered source. Failed, missing or unsupported proofs fall back to derivation.

The proof blob shares native blob replacement and deferred-page quarantine.
Its reference is published with the native header through the existing final
store persistence. WAL, protected staging, home sync and header ordering remain
unchanged. Legacy readers ignore the optional reference. Older writers cannot
preserve/reclaim an unknown proof blob; writer downgrades still require a
separately qualified recovery procedure.

Cost: changed-collection source hashing reads/encodes its raw rows once, with
one row's payload at a time and compact sorted ID references. Cold validation
also reads artifact bytes. Unchanged warm checkpoints retain the digest. This
is linear validation, not constant-cost cold open or fully incremental
index-page maintenance. Existing legacy stores still need their initial
derivation; this patch alone does not remove that remaining acceptance failure.

## Local regression results

The exact unchanged-engine regression and its fixture helpers were copied into
a separate worktree of `8e97eaaa346578b04353321dbb2f15f29de98e7d`. It fails on
`unchanged sources must not rebuild` before this correction. Both runs use the
same compiler/linker configuration and temporary-storage location; the candidate
checkout is not modified to run the baseline.

| Requirement | Test | Result |
| --- | --- | --- |
| Unchanged source | Three engine-only cold opens | Zero derived artifacts; all existing descriptors retained; exact frame equality against an independent rebuild |
| Real runtime boot | Cold runtime open | Only the changed `red_config` is rebuilt; unchanged artifacts retained |
| Acknowledged recovery | Process exit with pending WAL | Updated row recovered; old source proof rejected; rebuilt frame matches source |
| Interrupted publication | Process exit after primary-only persistence | New data survives; stale artifact rejected |
| Transaction projection | Process exit after checkpoint with an active or aborted transaction | Uncommitted row stays absent; artifacts containing its raw source are rejected |
| Corruption and legacy data | Wrong payload with a valid page, changed descriptor checksum, absent optional reference | Cold reuse rejected; subsequent publication restores a valid artifact/proof |
| Proof space lifecycle | 256 collection proofs across multiple pages; 12 updates | Same blob root and logical file length; shortened/removed pages quarantined and retirement survives reopen |
| Existing mutation guards | Native snapshot and full-text delta regressions | Manager replacement, raw changes, references and conservative fallbacks pass |
| File contract | Unknown versions and truncated payload/reference | Rejected; legacy padding has no reference |

These are local Rust tests, not signed-image or historical Auth/Platform
acceptance. The proof-space test checks logical file length and page retirement;
it does not replace separate allocated-block measurements in the full workload.
RC373 remains rejected for the historical Platform first publication (~123 s)
and physical-growth gate. No fleet rollout or database-volume downscale is
justified by these small tests. The same final artifact still needs both native
datasets, repeated writes/drain, held snapshots, crash recovery and shutdown,
under the retained 10-second and physical-growth criteria.

```sh
cargo test --locked -p reddb-io-file --lib native_artifact_sources
cargo test --locked -p reddb-io-server --lib native_source_tests
cargo test --locked -p reddb-io-server --lib native_source_proof_blob
cargo test --locked -p reddb-io-server --lib native_snapshot_tests
cargo test --locked -p reddb-io-server --lib delta_rejects_unproved_sources_references_revisions_and_size
```
