---
status: open
tag: AFK
gh: 525
---

# [AFK] gh-525: verify_chain endpoint + integrity-broken state

GitHub: reddb-io/reddb#525 (parent #521; needs #524)

## What to build

`POST /collections/:name/verify-chain` — admin-gated. Walks chain genesis -> tip, recomputes each hash, returns `{ checked, ok, first_bad_height: u64 | null }`.

On `ok: false`: collection metadata transitions to `integrity: 'broken'`. While broken, new INSERTs rejected `409 ChainIntegrityBroken`. Admin `POST /collections/:name/clear-integrity-flag` unblocks.

## Acceptance

- [ ] `POST /collections/:name/verify-chain` exists, admin-gated
- [ ] Returns `{checked, ok, first_bad_height}` with null when ok
- [ ] Walks in block_height order; recompute + compare hash
- [ ] Mismatch -> integrity=broken, persisted
- [ ] INSERT while broken -> 409 ChainIntegrityBroken
- [ ] Admin clear-integrity-flag endpoint (audited)
- [ ] Tests: intact ok, corrupt middle reports right height, corrupt tip reports tip, genesis-only ok
- [ ] Integration: verify_chain survives restart

## Notes

- `CARGO_TARGET_DIR=.target-gh525`
- Reuse storage/blockchain::verify_chain + runtime/blockchain_kind canonical encoder. Align them if drift caused #524's deferred test.
- Commit `Closes #525` if 8 acceptance pass; else `Refs`.

## Iteration note (2026-05-16, autonomous AFK)

Implementation drafted but NOT validated — `cargo` and `git` invocations were
denied at the agent's shell layer this session, so neither
`cargo check`/`cargo test` nor any commit could run. The code below is staged
on disk awaiting human-supervised build/commit.

Files changed:

- `crates/reddb-server/src/runtime/blockchain_kind.rs` — added
  `collect_blocks`, `verify_chain_outcome`, `VerifyChainOutcome`,
  `persist_integrity_flag`, `is_integrity_broken_persisted`. The
  `collect_blocks` helper rebuilds the canonical payload from `named` map,
  aligning the engine + verify-time encoders (the #524 deferred drift point).
- `crates/reddb-server/src/runtime/impl_dml.rs` — added
  `verify_chain_for_collection`, `clear_chain_integrity_flag`,
  `is_chain_integrity_broken`. INSERT path now early-returns
  `ChainIntegrityBroken` when the flag is set.
- `crates/reddb-server/src/runtime.rs` — `RuntimeInner` field
  `chain_integrity_broken: Mutex<HashMap<String, bool>>` (in-memory mirror,
  lazy-loaded from `red_config` on cold start).
- `crates/reddb-server/src/runtime/impl_core.rs` — init the new field.
- `crates/reddb-server/src/server/routing.rs` — `POST
  /collections/:name/verify-chain` and `.../clear-integrity-flag` handlers
  gated behind `RED_ADMIN_TOKEN` (free when unset, dev parity).
- `crates/reddb-server/tests/runtime_blockchain_verify_chain.rs` — 8 cases
  (genesis-only ok, intact multi-block ok, corrupt middle, corrupt tip,
  insert blocked + admin clear unblocks, persisted flag round-trip, non-chain
  returns None, canonical encoder alignment regression).

Blockers for next iteration:

1. Run `CARGO_TARGET_DIR=.target-gh525 cargo check -p reddb-server` and
   `cargo test -p reddb-server runtime_blockchain_verify_chain` to confirm
   the new test file compiles + passes.
2. Tighten the admin-clear endpoint to record an audit event before committing
   (the acceptance criterion says "audited"). Today it only mutates state +
   returns ok; wire `audit_log.emit(...)` analogous to other admin paths.
3. Add a true post-restart test: re-open the same on-disk RedDB instance and
   confirm `is_chain_integrity_broken` returns true without re-running verify.
   The current in-memory test relies on the lazy reload path but does not
   exercise drop+reopen.
4. Commit per the issue template once builds are green.
