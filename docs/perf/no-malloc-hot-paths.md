# No-Malloc Hot-Path Ratchet

ADR 0073 section 3 is enforced by a measured test ratchet, not by a lexical
lint. The checked-in manifest lives in
`crates/reddb-server/src/storage/no_malloc_hot_paths.rs` as
`COVERED_OPERATIONS`; each entry names a storage data-plane operation, its
allowed allocation floor, and any temporary exception.

The current covered operations are:

| Operation | Floor | Exception |
| --- | ---: | --- |
| `hash-index-point-read-hit` | 0 | None |
| `growing-segment-flat-row-insert` | 3 | Temporary floor tracked under #1956; `bulk_insert` returns an allocated id vector and builds per-call flat insert bookkeeping. |
| `page-cache-hit` | 0 | None |
| `blob-cache-l1-hit` | 2 | Temporary floor tracked under #1956; `BlobCache::get` builds the owned namespace/key lookup key on the hit path. |
| `wal-record-encode-into-group-commit-buffer` | 0 | None |

## Adding Coverage

Add one operation at a time. Warm all setup structures before entering
`measure_allocations`; the measured closure must contain only the operation
whose per-op cost is being ratcheted. Start with a floor of `0`, run the
targeted test, and keep the deliberate red result while deciding whether to fix
the allocation or record the measured floor.

The bar for a nonzero floor is the same as loosening a lint ratchet: the entry
must name the reason and link a follow-up issue. Removing an operation or
raising a floor needs the same justification. Lowering a floor is encouraged
whenever a covered path is fixed forward.

## CI Contract

The ratchet runs in the normal Rust test lane because it is a `reddb-io-server`
unit test. A regression that adds a `Box::new`, `format!`, `Vec::with_capacity`,
or any other heap allocation inside a covered measured closure fails with the
operation name and the measured allocation count.
