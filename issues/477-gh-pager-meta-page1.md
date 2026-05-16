---
status: open
tag: AFK
gh: 477
---

# [AFK] gh-477: Fold pager meta (free list) into page 1 of datafile (feature flag)

GitHub: reddb-io/reddb#477

## What to build

Feature flag `fold_pager_meta`. When ON: pager meta in page 1 (+ overflow chain when needed); no `-meta` sidecar. OFF: current behavior. Free list overflow works > N pages (N defined in ADR). Tests cover massive allocation forcing overflow. ADR updates pages 0/1 layout.

## Acceptance criteria

- [ ] Flag `fold_pager_meta` controls behavior
- [ ] ON: pager meta in page 1 (+ overflow chain); `-meta` not created
- [ ] OFF: current behavior preserved
- [ ] Free list overflow works > N pages
- [ ] Tests cover massive allocation forcing overflow
- [ ] ADR updates page 0/1 layout

## Notes
- `CARGO_TARGET_DIR=.target-gh477`
- Commit `Closes #477` or `Refs` if partial

## Iteration 1 — 2026-05-16

Implemented (un-verified by `cargo build` — sandbox blocked all
`cargo`/`git` invocations this iteration):

- `physical::set_fold_pager_meta_enabled` / `fold_pager_meta_enabled`
  (process-global toggle mirroring meta-json + seqn-journal patterns).
  Env escape hatch `REDDB_FOLD_PAGER_META=1`. Re-exported from
  `reddb` umbrella + `reddb_server` lib.
- `pager::write_meta_shadow` honours the toggle: when ON it suppresses
  the `<data>-meta` shadow write and best-effort-removes any stale
  shadow on disk.
- `impl_pages.rs`: page-1 metadata writer factored into
  `build_meta_page1_with_overflow`. Single-page metadata bytes
  unchanged from the legacy `RDM2` layout. When the serialised payload
  exceeds 4064 bytes the writer emits an `RDM3` wrapper on page 1
  chaining `PageType::Overflow` pages. The loader assembles via
  `read_meta_payload` and feeds the same parser.
- `free_existing_overflow_chain` frees the old `RDM3` overflow chain
  before each meta rewrite so the trail doesn't leak.
- Tests in `tests/e2e_fold_pager_meta_policy.rs`:
  - `fold_off_default_preserves_meta_shadow` (OFF: `-meta` present).
  - `fold_on_skips_meta_shadow_and_data_round_trips` (ON: `-meta`
    absent; data + catalog round-trip via page 1 only).
  - `massive_catalog_forces_meta_overflow_chain` (200 collections force
    the chain; every collection survives reopen).
  - `freelist_trunk_chain_handles_many_pages` (> 3×
    `FREE_IDS_PER_TRUNK` ids exercise multi-trunk reload).
- ADR 0018: new section "Folded pager meta — page 0/1 layout (gh-477)"
  documents single-page and overflow-form byte layout + free-list
  trunk chain semantics.

### Blockers / next iteration

- `cargo build`, `cargo check`, `cargo test`, and `git` all required
  approval in the harness this run. Need a follow-up pass to actually
  compile + run the new e2e tests against `CARGO_TARGET_DIR=.target-
  gh477`. The code is small/contained but un-compiled.
- Tier-driven auto-enable (flip `fold_pager_meta` on at `Max`) still
  blocked on the shared `RuntimeOptions` / `StorageLayout` wiring
  same as #471/#472/#473/#475. This slice ships the opt-in policy
  only.
