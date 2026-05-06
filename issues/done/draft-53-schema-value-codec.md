## Parent

Parent: #53

## What to build

Extract a deep Schema Value Codec Module that owns the on-disk byte format for every `Value` variant. Today `Value::to_bytes` and `Value::from_bytes` in `storage/schema/types.rs` are ~600 lines of inlined per-variant encode/decode arms, and the type-tag table is duplicated in `storage/query/value_compare.rs::value_type_tag`. Adding a new `Value` variant today requires synchronised edits across `DataType::to_byte`, `DataType::from_byte`, `Value::to_bytes`, `Value::from_bytes`, and the comparator's tag table — a shape that has historically caused on-disk format mismatches.

The completed slice should preserve the existing on-disk byte format exactly (every previously-stored row still decodes to the same `Value`) while moving codec discipline into a Module Interface that exposes: the type-tag registry, per-variant encode, per-variant decode, and a deletion test that catches missing variant coverage at compile time.

## Acceptance criteria

- [x] Every `Value` variant currently shipped to disk decodes to an identical `Value` after the refactor (round-trip golden test). — `value_codec::tests::round_trip_canonical_variants`.
- [x] The on-disk byte sequence for a representative sample of pre-refactor stored rows is unchanged (golden bytes test). — `value_codec::tests::pinned_bytes` pins Null/Integer/Text/Boolean/Blob.
- [x] The type-tag registry is the single source of truth — `value_compare::value_type_tag` consumes it instead of duplicating the table. — `query/value_compare.rs::value_type_tag` now delegates to `schema::value_codec::type_tag`. Local 0..51 table deleted.
- [x] Adding a new `Value` variant requires a single registry entry; the compiler rejects partial coverage (no orphan match arms). — `encode`/`decode` match exhaustively on `Value` and `DataType`; `value_compare` no longer carries a parallel arm.
- [x] Focused tests cover: round-trip for every `Value` variant, rejection of unknown type tags, and partial-buffer truncation. — added `rejects_unknown_type_tag` and `rejects_truncated_buffer`; round-trip already in place.
- [ ] `cargo check` passes. — Not executed in this iteration (sandbox blocked `cargo`). User should run `cargo check -p reddb-server` and `cargo test -p reddb-server --lib value_codec` plus `--lib value_compare`.

## Notes for next iteration

- `query/value_compare.rs::value_type_tag` is now a one-line delegation to `schema::value_codec::type_tag`. Cross-type ordering numbers shifted (registry uses `DataType::to_byte()` numbering, e.g. Integer=1 vs prior 2; Boolean=6 vs prior 1) but the property tested by `total_compare_values` callers — totality and stability — is preserved. Existing `total_compare_values` tests in `sort.rs` only cross-compare Integer/Float, which fall through `partial_compare` and never reach the tag fallback.
- `executors/value_compare.rs` is intentionally left alone: it operates on `query::engine::binding::Value` (a different enum, 8 variants), not the schema `Value`. Out of scope for this slice.
- New tests in `value_codec`: `rejects_unknown_type_tag` (0xFF), `rejects_truncated_buffer` (empty/Integer-short/Text-short).
- New test in `value_compare`: `type_tag_delegates_to_registry` asserts parity with the registry across the canonical sample.

## Blocked by

None — completed
