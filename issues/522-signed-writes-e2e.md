---
status: open
tag: AFK
gh: 522
---

# [AFK] gh-522: Signed Writes end-to-end — registry + insert verification

GitHub: reddb-io/reddb#522 (parent #520)

## Build on top of iter 1 of #520

Parser already accepts `CREATE COLLECTION ... SIGNED_BY (...)` and `ALTER COLLECTION ... { ADD | REVOKE } SIGNER`. This slice wires the runtime.

## Acceptance

- [ ] DDL persists initial registry on collection schema
- [ ] ALTER ADD/REVOKE SIGNER is admin-gated; appends to `signer_history` (actor + ts)
- [ ] Reserved cols `signer_pubkey` (32B) + `signature` (64B) auto-added when signed_writes on
- [ ] Insert without signer_pubkey/signature → 400 MissingSignatureFields
- [ ] Insert with unknown signer → 401 UnknownSigner
- [ ] Insert with bad signature → 401 InvalidSignature
- [ ] Insert with revoked signer → 401 RevokedSigner; past records still readable + re-verifiable
- [ ] `signer_pubkey` queryable via WHERE
- [ ] e2e test covers full demoable flow

## Notes

- `CARGO_TARGET_DIR=.target-gh522`
- Use ed25519-dalek. Add to Cargo.toml if missing.
- Canonical encoding: reuse engine's existing content-hash encoder.
- Commit `Closes #522` if all acceptance pass; else `Refs #522`.

## Iter 1 progress (tracer bullet — pure primitives)

Lands the audited, side-effect-free core that the runtime wiring in
iter 2 will sit on top of:

- `crates/reddb-server/src/storage/signed_writes.rs` (new, ~+460
  lines incl. tests): `SignerRegistry { allowed, history }`,
  `SignerHistoryEntry { action: Add|Revoke, pubkey, actor, ts }`,
  `verify_insert(registry, fields, canonical_payload)` returning the
  full `SignedWriteError` taxonomy the issue specifies
  (`MissingSignatureFields`, `UnknownSigner`, `RevokedSigner`,
  `InvalidSignature`, `MalformedSignerPubkey`,
  `MalformedSignature`), plus `reverify_row` for integrity scans.
  Reserved column names + lengths exported as constants.
- 11 unit tests cover: from-initial seeding history, idempotent
  add, revoke-then-blocked, missing fields (both / one), unknown
  signer, revoked-distinguished-from-unknown, valid signature
  accepted, tampered payload → InvalidSignature, malformed length
  for sig + pubkey, **past-record-still-reverifies-after-revoke**
  (acceptance criterion), display-string stability.
- `Cargo.toml`: ed25519-dalek = "2" (already in Cargo.lock 2.2.0).
- `storage/mod.rs`: exposes `pub mod signed_writes`.

NOT YET DONE (iter 2):

- `execute_create_collection` does not consume `allowed_signers`
  yet — it only handles `KIND graph|document|metrics`. Need to
  persist the registry onto the collection contract (likely a new
  optional `signer_registry: SignerRegistry` on
  `physical::CollectionContract`) and surface it from
  `catalog_model_snapshot`.
- ALTER COLLECTION ADD/REVOKE SIGNER not yet parsed (no AST node
  for it — `core.rs::AlterOperation` has none). Despite the #520
  commit message, the parser only accepts `SIGNED_BY` on CREATE.
  Iter 2 must land an `AlterSignerRegistry { add: bool, pubkey }`
  AST node + parser branch + admin-gate execution.
- Reserved-column injection on the schema for signed collections.
- Wire `verify_insert` into the INSERT path inside `impl_core`
  (likely in the mutation pipeline before the row is appended to
  the underlying storage).
- HTTP error mapping: `SignedWriteError → 400/401` in the wire
  layer.
- e2e test creating a signed collection, inserting a valid row,
  inserting one with each error class, revoking a signer, and
  re-verifying the historical row.

cargo not run in this environment (sandbox blocked); rely on CI to
type-check. Module is self-contained, only depends on already-vendored
crates (`ed25519-dalek` already resolved in Cargo.lock 2.2.0).
