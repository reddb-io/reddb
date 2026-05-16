---
status: open
tag: AFK
gh: 523
---

# [AFK] gh-523: Blockchain Foundation — DDL + immutable mutate gate + genesis block

GitHub: reddb-io/reddb#523 (parent #521)

## What to build

Foundation slice. No chain validation yet. Just kind persistence, reserved cols, mutate gate, and auto-genesis.

## Acceptance

- [ ] DDL `CREATE COLLECTION name KIND blockchain` persists `kind: 'chain'` on schema
- [ ] Reserved cols: `block_height` (u64), `prev_hash` (bytes 32), `timestamp` (u64 ms), `hash` (bytes 32)
- [ ] ALTER COLLECTION cannot change kind
- [ ] Genesis row auto-inserted at creation: block_height=0, prev_hash=[0;32], empty payload, hash=sha256(canonical(...))
- [ ] UPDATE on any row → 409 BlockchainCollectionImmutable
- [ ] DELETE → 409 BlockchainCollectionImmutable
- [ ] INSERT accepts records, engine computes hash (no validation yet)
- [ ] Other kinds unaffected
- [ ] Tests cover kind persistence, genesis, mutate gate, INSERTs

## Notes

- `CARGO_TARGET_DIR=.target-gh523`
- Reuse storage/blockchain.rs from #521 iter 1 for hash helpers.
- Reuse storage/blockchain.rs from #521 - it has the hash helpers.
- Commit with `Closes #523` if all 8 acceptance pass; else `Refs`.

## Iter status (2026-05-16)

Foundation already wired in working tree (uncommitted on entry):
- `runtime/blockchain_kind.rs` — kind marker (`red.collection.{name}.kind = "chain"`),
  reserved columns (`block_height` u64, `prev_hash` Blob[32], `timestamp` u64 ms,
  `hash` Blob[32]), `chain_tip` scan, `make_block_reserved_fields`, `genesis_fields`,
  `canonical_payload` (sorted, reserved-cols-skipped).
- `runtime/impl_ddl.rs` — `CREATE COLLECTION ... KIND blockchain` routes to
  Table model + calls `install_blockchain_kind` (marks kind, inserts genesis row
  at height 0 with prev_hash=[0;32]).
- `runtime/impl_dml.rs` — INSERT strips user-supplied reserved cols and auto-fills
  engine-computed `(block_height, prev_hash, timestamp, hash)`. UPDATE + DELETE
  return `RedDBError::InvalidOperation("BlockchainCollectionImmutable: ...")`
  (HTTP layer maps this to 409).
- `AlterOperation` enum has no `SetKind` variant — kind changes are unreachable
  by construction.

Added this iter:
- `tests/runtime_blockchain_kind.rs` — integration tests for kind persistence
  (via genesis presence), genesis row shape, hash chaining across INSERTs,
  UPDATE/DELETE rejection with `BlockchainCollectionImmutable` substring match,
  user-supplied reserved-column overwrite, and "other kinds unaffected".

Blocker: harness sandbox in this worktree rejects every `git` / `cargo`
invocation ("This command requires approval") and the AskUserQuestion path
returned no answer. Could not run `cargo test` to confirm the new test file
compiles + passes, and could not stage/commit. Operator: please run
`CARGO_TARGET_DIR=.target-gh523 cargo test -p reddb-server --test
runtime_blockchain_kind` and, on green, commit the working-tree changes
(`runtime.rs` adds `blockchain_kind` mod; `impl_ddl.rs` + `impl_dml.rs` add
the gates; `blockchain_kind.rs` + `tests/runtime_blockchain_kind.rs` are new).

## Iter 2 attempt (2026-05-16)

Same sandbox blocker confirmed — `cargo`, `git`, even `cargo --version`
all return "This command requires approval". No code changes this iter.
Reviewed `tests/runtime_blockchain_kind.rs` against the public API surface
(`RedDBRuntime::with_options`, `RedDBOptions::in_memory`, `execute_query`,
`RuntimeQueryResult.result.records`, `Value::Blob/UnsignedInteger/Integer`,
`RedDBError::InvalidOperation`) — shape looks consistent with the imports
the file declares. Still needs an unsandboxed run before commit.
