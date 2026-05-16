---
status: open
tag: AFK
gh: 521
---

# [AFK] gh-521: Blockchain Collection Kind — hash-chained append-only + verify_chain + chain-tip

GitHub: reddb-io/reddb#521

## What to build

New collection kind: `KIND blockchain`. Append-only. Each row hashes to previous row's hash (`prev_hash`). Engine assigns `block_height` + `timestamp`. Exposes `verify_chain()` and `GET /collections/:name/chain-tip`.

## DDL

- `CREATE COLLECTION audit_log KIND blockchain` — declares the kind. Composes with `SIGNED_BY (...)` from #520.

## Reserved columns (engine-managed, immutable after insert)

- `block_height: u64` — monotonic per collection
- `prev_hash: bytes(32)` — sha256 of previous block; zero hash for genesis
- `timestamp: u64` — ms since epoch
- `hash: bytes(32)` — sha256(canonical(prev_hash || block_height || timestamp || payload [|| signer_pubkey || signature]))

## Insert pipeline

1. Client submits `(payload, prev_hash)` plus optional `(signer_pubkey, signature)` when Signed Writes enabled.
2. Engine checks `prev_hash == current_tip.hash`; reject with `409 BlockchainConflictRetry` if stale.
3. Assign `block_height = tip.block_height + 1`, `timestamp = now()`.
4. Compute `hash = sha256(canonical(prev_hash || block_height || timestamp || payload [|| sig fields]))`.
5. Store row with reserved cols.

## Acceptance

- [ ] Parser accepts `CREATE COLLECTION name KIND blockchain` (+ optional SIGNED_BY)
- [ ] Genesis block inserts with `prev_hash = 0x000...`, `block_height = 0`
- [ ] Second block reads tip, supplies prev_hash, gets `block_height = 1`
- [ ] Stale `prev_hash` returns `409 BlockchainConflictRetry`
- [ ] UPDATE/DELETE on blockchain collection returns `409 BlockchainCollectionImmutable`
- [ ] `verify_chain()` walks the chain, returns Ok or first-inconsistency report
- [ ] `GET /collections/:name/chain-tip` returns `(block_height, tip_hash, timestamp)`
- [ ] Test: 5-block chain, verify_chain Ok; corrupt block 2's payload, verify_chain reports block 2

## Notes

- `CARGO_TARGET_DIR=.target-gh521`
- Hybrid integration: collection schema flag `kind: 'chain'`, NOT a new EntityKind variant.
- Reuse existing sha256 + canonical encoder.
- Commit with `Closes #521` if all 8 acceptance bullets pass; else `Refs #521`.
- Large slice — partial OK; land parser + genesis + verify_chain minimum.

## Iter 2 progress (uncommitted — blocked on exec permissions)

Done locally (files modified, NOT committed because cargo/git execution
requires approval that the AFK runner did not surface):

- `crates/reddb-server/src/storage/blockchain.rs` (new) — pure-logic module:
  - `GENESIS_PREV_HASH`, `Block`, `SignedFields`, `ChainTip`
  - `BlockchainError::{ConflictRetry, Immutable}` with Display impls
  - `compute_block_hash(prev_hash, height, ts, payload, Option<&SignedFields>)`
    using length-prefixed canonical encoding (sha256 over
    `prev_hash || u64_be(height) || u64_be(ts) || u64_be(payload_len) || payload
     [|| signer_pubkey || u64_be(sig_len) || sig]`)
  - `verify_chain(&[Block]) -> VerifyReport::{Ok, Inconsistent{block_height, reason}}`
    — walks chain, checks height monotonicity, prev_hash linkage, recomputed hash
  - 7 unit tests including 5-block Ok + tamper-block-2 reports height 2
- `crates/reddb-server/src/storage/mod.rs` — `pub mod blockchain;`
- `crates/reddb-server/src/storage/query/parser/tests.rs` — 2 new asserts
  proving `CREATE COLLECTION audit_log KIND blockchain` and the
  `KIND blockchain SIGNED_BY ('hex')` combo parse cleanly (parser already
  accepts arbitrary KIND post-#520; tests pin the contract).

Acceptance status after iter 2 (files-on-disk):
- [x] Parser accepts `CREATE COLLECTION name KIND blockchain` (+ SIGNED_BY)
- [ ] Genesis block inserts (engine wiring still TODO)
- [ ] Second block reads tip (engine wiring still TODO)
- [ ] Stale prev_hash → 409 ConflictRetry (error type defined, not wired)
- [ ] UPDATE/DELETE → 409 Immutable (error type defined, not wired)
- [x] `verify_chain()` pure-logic + tests
- [ ] `GET /collections/:name/chain-tip` HTTP route
- [x] 5-block chain Ok + corrupt block 2 reported (covered by unit tests)

### Blocker for next iter

The runner cannot exec `cargo` / `git` to verify or commit. Next iteration
must:
1. Run `CARGO_TARGET_DIR=.target-gh521 cargo check -p reddb-server` and
   `cargo test -p reddb-server --lib blockchain` + parser tests.
2. Commit iter 2 files with `Refs #521`.
3. Then begin iter 3: wire executor side — collection schema flag `kind: 'chain'`
   on `CreateCollection` handler, blockchain INSERT path computing reserved
   columns from `compute_block_hash`, ChainTip query helper, HTTP route,
   UPDATE/DELETE gating returning `BlockchainCollectionImmutable`.
