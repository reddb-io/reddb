---
status: open
tag: AFK
gh: 473
---

# [AFK] gh-473: Make seq-N catalog journal opt-in; off-default outside max tier

GitHub: reddb-io/reddb#473

## What to build

Seq-N catalog journal only written when override resolved = true. Default per tier: max=on, others=off. Retention: 32 in max, 4 when toggled on elsewhere. Passive recovery for 3 states: present, absent, corrupt. `reddb status` shows journal state + retention.

## Acceptance criteria

- [ ] Journal only written when override resolved = true
- [ ] Default per tier: max=on, others=off
- [ ] Retention 32 / 4
- [ ] Passive recovery 3 states tested
- [ ] `reddb status` shows journal state + retention
- [ ] Existing recovery tests still pass

## Notes
- `CARGO_TARGET_DIR=.target-gh473`
- Commit `Closes #473` or `Refs` if partial
- Tier wiring still absent. Land the type/policy half; defer runtime auto-enable.

## Iteration 1 (2026-05-16, partial)

Type/policy half landed; tier auto-enable still deferred (same blocker as
#471/#472 — no RuntimeOptions plumbing yet).

Changes:
- `physical.rs`: new process-global `SEQN_JOURNAL_POLICY` (AtomicU8) +
  `SEQN_JOURNAL_RETENTION` (AtomicUsize) mirroring the meta-json sidecar
  toggle. New public surface: `set_seqn_journal_enabled`,
  `seqn_journal_enabled`, `set_seqn_journal_retention`,
  `seqn_journal_retention`. Env escape hatches `REDDB_SEQN_JOURNAL=1` and
  `REDDB_SEQN_JOURNAL_RETENTION=<N>`. New constant
  `OPT_IN_METADATA_JOURNAL_RETENTION = 4`; existing
  `DEFAULT_METADATA_JOURNAL_RETENTION = 32` retained for max tier.
- `physical/metadata_file.rs`: `save_for_data_path` skips the seq-N copy
  unless `seqn_journal_enabled()` returns true. `prune_journal_for_data_path`
  uses runtime-resolved retention rather than hardcoded 32 — so opt-in
  outside max trims to 4. Recovery path unchanged: still tries binary →
  journals (reversed) → JSON sidecar.
- `lib.rs`: re-export the four new fns + both retention constants.
- `src/bin/red.rs` (status): adds `seqn_journal: {enabled, retention}` to
  both JSON and text output.
- `tests/e2e_seqn_journal_policy.rs`: three tests covering
  (a) default-off — no journal entries written; binary still present,
  (b) opt-in with retention 4 — journals appear, bounded by retention,
  (c) recovery 3-state — present (loads from binary), corrupt (heals
  from journal entry), absent (returns io::Error).

Acceptance progress:
- [x] Journal only written when override resolved = true
- [ ] Default per tier wiring (max=on/others=off) — blocked on tier plumbing
- [x] Retention 32 / 4 (constants + setter; tier plumbing flips)
- [x] Passive recovery 3 states tested
- [x] `reddb status` shows journal state + retention
- [ ] Existing recovery tests still pass — not verified locally
  (cargo/git blocked in this session)

Blockers / next iteration:
- Verify `cargo test -p reddb-server` and the new e2e test pass.
- Same RuntimeOptions plumbing as #471/#472 is needed to call
  `set_seqn_journal_enabled(true)` + `set_seqn_journal_retention(32)`
  for the `Max` tier at runtime startup.
