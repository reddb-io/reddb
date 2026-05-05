## Parent

Parent: #53

## What to build

Extract a deep Schema Value Codec Module that owns the on-disk byte format for every `Value` variant. Today `Value::to_bytes` and `Value::from_bytes` in `storage/schema/types.rs` are ~600 lines of inlined per-variant encode/decode arms, and the type-tag table is duplicated in `storage/query/value_compare.rs::value_type_tag`. Adding a new `Value` variant today requires synchronised edits across `DataType::to_byte`, `DataType::from_byte`, `Value::to_bytes`, `Value::from_bytes`, and the comparator's tag table — a shape that has historically caused on-disk format mismatches.

The completed slice should preserve the existing on-disk byte format exactly (every previously-stored row still decodes to the same `Value`) while moving codec discipline into a Module Interface that exposes: the type-tag registry, per-variant encode, per-variant decode, and a deletion test that catches missing variant coverage at compile time.

## Acceptance criteria

- [ ] Every `Value` variant currently shipped to disk decodes to an identical `Value` after the refactor (round-trip golden test).
- [ ] The on-disk byte sequence for a representative sample of pre-refactor stored rows is unchanged (golden bytes test).
- [ ] The type-tag registry is the single source of truth — `value_compare::value_type_tag` consumes it instead of duplicating the table.
- [ ] Adding a new `Value` variant requires a single registry entry; the compiler rejects partial coverage (no orphan match arms).
- [ ] Focused tests cover: round-trip for every `Value` variant, rejection of unknown type tags, and partial-buffer truncation.
- [ ] `cargo check` passes.

## Blocked by

None - can start immediately
