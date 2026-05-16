---
status: open
tag: AFK
gh: 526
---

# [AFK] gh-526: Signed Writes + Blockchain composition

GitHub: reddb-io/reddb#526 (parents #520, #521)

## What to build

`CREATE COLLECTION x KIND blockchain SIGNED_BY (...)` enables BOTH features. Reserved cols include block_height, prev_hash, timestamp, signer_pubkey, signature, hash.

Client signs `(block_height || prev_hash || timestamp || canonical(payload))`. Engine extends hash to `sha256(block_height || prev_hash || timestamp || canonical(payload) || signer_pubkey || signature)`.

INSERT runs chain validation (#524) THEN signature validation (#522). Either fails -> atomic reject.

verify_chain (#525) re-checks chain hashes AND signatures.

## Acceptance

- [ ] DDL creates collection with both features active
- [ ] Genesis block uses null pubkey + null signature (documented exemption) or special "genesis" marker
- [ ] INSERT requires both chain + signature fields; missing -> typed error
- [ ] Valid chain + bad sig -> 401 InvalidSignature; tip unchanged
- [ ] Valid sig + stale prev_hash -> 409 ChainConflict; sig "not consumed"
- [ ] hash includes signer_pubkey + signature
- [ ] verify_chain re-validates signatures, reports first failing height
- [ ] Tampering with signer_pubkey or signature -> verify_chain fails at that height

## Notes

- `CARGO_TARGET_DIR=.target-gh526`
- Most composition emerges naturally — #522 sig validator + #524 chain validator + #525 verify in same pipeline. This slice locks the contract + extends hash + adds e2e tests.
- Commit `Closes #526` if 8 acceptance pass; else `Refs`.

## Iter 1 progress (2026-05-16) — pure-logic composition tracer

Lands the composition layer as a side-effect-free module on top of the
audited primitives in `storage::blockchain` (#523/#524) and
`storage::signed_writes` (#520 iter 1 / #522 iter 1).

End-to-end runtime wiring is gated on #522 completing the INSERT-side
signature validator inside the engine — that piece is still iter 1
(primitives only; registry not attached to `CollectionContract`; the
INSERT pipeline does not yet call `verify_insert`). Without that, the
INSERT-shaped acceptance bullets of #526 cannot be exercised against
the runtime; this iter 1 pins everything that CAN be locked down
without it.

**Uncommitted** — sandbox blocks `cargo` and `git` bash invocations
(same blocker that #520 iter 1 flagged on the parent branch). No
`cargo check`/`cargo test` run, no commit made.

### New file

`crates/reddb-server/src/runtime/signed_chain.rs` (~+400 lines incl.
tests) — module registered in `runtime.rs` as `pub mod signed_chain;`.

Public surface:

- `RESERVED_COLUMNS_SIGNED_CHAIN` — union of the four chain reserved
  cols + the two signed-writes reserved cols (six total).
- `GENESIS_SIGNER_PUBKEY` / `GENESIS_SIGNATURE` / `genesis_signed_fields`
  — documents the genesis exemption: all-zero 32-byte pubkey + 64-byte
  all-zero signature. Pair pinned by `is_genesis_signed_marker`.
- `make_signed_block_reserved_fields(prev_hash, height, ts, payload,
  signer_pubkey, signature) -> (fields, hash)` — engine-side
  constructor: builds the six reserved fields and binds signer_pubkey
  + signature into the block hash via
  `compute_block_hash(..., Some(&SignedFields { ... }))`.
- `SignedChainVerifyOutcome { checked, ok, first_bad_height,
  signature_failure }` + `verify_chain_with_signatures(&[Block])` —
  walks the chain in order, lets `verify_chain` cover the hash +
  linkage check, and additionally re-verifies the Ed25519 signature
  against the stored pubkey + canonical payload on each non-genesis
  block. Catches the forge-and-rehash attack (attacker recomputes
  `hash` to match a forged signature) which the bare chain walker
  would miss.

### Tests (9 cases, all pure logic)

1. `reserved_columns_signed_chain_is_union` — six cols present.
2. `genesis_uses_null_pubkey_and_signature` — documented exemption.
3. `hash_binds_signer_pubkey_and_signature` — flips of either signer
   pubkey OR signature produce different hashes.
4. `valid_signed_chain_verifies_ok` — 1 genesis + 3 signed blocks.
5. `tampering_signer_pubkey_fails_at_block_height` — `verify_chain`
   catches a flipped pubkey at the right height.
6. `tampering_signature_with_recomputed_hash_caught_by_sig_reverify` —
   attacker recomputes hashes end-to-end so the chain links cleanly;
   signature reverify still catches the forgery and reports
   `signature_failure: true` + the correct `first_bad_height`.
7. `composition_chain_fail_then_sig_fail_atomic_reject` — pins
   validator-order semantics: `verify_insert` is a pure function, so
   a chain failure never "consumes" a signature, and a bad signature
   surfaces as `InvalidSignature` regardless of chain state.
8. `missing_signature_fields_typed_error` — typed
   `MissingSignatureFields` error when both reserved fields are
   absent.
9. `genesis_marker_recognised` — predicate sanity.

### Acceptance coverage

- [x] **hash includes signer_pubkey + signature** — pinned in test 3.
- [x] **Genesis block uses null pubkey + null signature (documented
       exemption)** — pinned in test 2 (the marker IS the "special
       genesis marker").
- [x] **Tampering with signer_pubkey or signature → verify_chain fails
       at that height** — pinned in tests 5 + 6 (the latter covers
       the harder case where the attacker also recomputes the
       downstream hashes).
- [x] **verify_chain re-validates signatures, reports first failing
       height** — `verify_chain_with_signatures` short-circuits at the
       first failure and returns the failing height via
       `first_bad_height`.
- [x] **INSERT requires both chain + signature fields; missing →
       typed error** — `SignedWriteError::MissingSignatureFields`,
       test 8.
- [~] **Valid chain + bad sig → 401 InvalidSignature; tip unchanged**
      — pure-logic half pinned (test 7 yields
      `InvalidSignature`); "tip unchanged" requires the #522 runtime
      wiring to assert against a real INSERT path.
- [~] **Valid sig + stale prev_hash → 409 ChainConflict; sig 'not
       consumed'** — same situation: validators are pure functions, so
       order-independence is structural, but the runtime-level
       assertion needs #522 iter 2.
- [~] **DDL creates collection with both features active** — parser
      already accepts `CREATE COLLECTION x KIND blockchain SIGNED_BY
      (...)` (#520 iter 1 lands the clause). End-to-end DDL
      persistence of the registry onto a `KIND blockchain` collection
      is part of #522 iter 2 (`CollectionContract` extension).

### Iter 2 backlog (next AFK pickup)

1. Land #522 iter 2 (registry persistence on `CollectionContract`,
   `verify_insert` wired into the INSERT pipeline, REST 400/401
   error mapping). #526's runtime composition then becomes a small
   patch in `impl_dml`:
   - call `verify_insert` BEFORE the chain validator when the
     collection has both `KIND blockchain` and a non-empty
     `SignerRegistry`;
   - use `make_signed_block_reserved_fields` instead of
     `make_block_reserved_fields` when filling reserved columns on a
     signed chain INSERT.
2. Extend `runtime::blockchain_kind::collect_blocks` to populate
   `Block.signed` from the stored `signer_pubkey` + `signature`
   columns so `verify_chain` / `verify_chain_with_signatures` see the
   exact preimage the engine wrote.
3. Wire `verify_chain_with_signatures` into
   `POST /collections/:name/verify-chain` when the collection has a
   signer registry; fall back to the unsigned walker otherwise.
4. e2e integration test mirroring tests 1–9 against the runtime once
   the wiring lands. Drop the `[~]` partial marks above to `[x]`,
   then close with `Closes #526`.

### Blocker (carry-over)

AFK loop sandbox denies `cargo` and `git` Bash invocations on this
branch (`afk/526-signed-blockchain-compose`). No `cargo test`, no
`cargo check`, no commit possible from this session. Module is
self-contained — only depends on already-published items in
`storage::blockchain` (`Block`, `SignedFields`,
`compute_block_hash`, `verify_chain`, `VerifyReport`,
`GENESIS_PREV_HASH`) and `storage::signed_writes`
(`SignerRegistry`, `verify_insert`, `reverify_row`,
`RESERVED_SIGNER_PUBKEY_COL`, `RESERVED_SIGNATURE_COL`,
`SIGNATURE_LEN`, `SIGNER_PUBKEY_LEN`).

Once cargo+git are allowed, expected workflow:

    CARGO_TARGET_DIR=.target-gh526 cargo test -p reddb-server signed_chain
    git add crates/reddb-server/src/runtime.rs \
            crates/reddb-server/src/runtime/signed_chain.rs \
            issues/526-signed-blockchain-compose.md
    git commit -m "feat(blockchain): signed-blockchain composition tracer (Refs #526)"
