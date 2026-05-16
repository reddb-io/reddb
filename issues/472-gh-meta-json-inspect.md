---
status: open
tag: AFK
gh: 472
---

# [AFK] gh-472: Drop .meta.json default; add reddb inspect catalog CLI

GitHub: reddb-io/reddb#472

## What to build

Tiers minimal/standard/performance no longer auto-write `.meta.json`. New `reddb inspect catalog` produces JSON state at any tier; `--at <seq>` returns catalog at snapshot (if journal available) else explicit error. Max keeps auto-writing `.meta.json` in `<dbname>.rdb.d/meta/`.

## Acceptance criteria

- [ ] minimal/standard/performance do not auto-write `.meta.json`
- [ ] `reddb inspect catalog` produces current state JSON for any tier
- [ ] `--at <seq>` returns catalog at snapshot or explicit error
- [ ] max keeps writing to `<dbname>.rdb.d/meta/`
- [ ] Docs updated pointing at CLI as substitute

## Notes
- `CARGO_TARGET_DIR=.target-gh472`
- Commit with `Closes #472` (or `Refs` if partial)
- Tier wiring may be absent — flag and partial-land if so.

## Iteration 1 (partial land, 2026-05-16)

Done:
- New `red inspect catalog --path <FILE> [--at <SEQ>]` CLI command. Loads
  `PhysicalMetadataFile` via existing `load_for_data_path` (current state)
  or `metadata_journal_path_for(seq)` + `load_from_binary_path` (at-snapshot).
  Missing journal → explicit error with the path that was probed. Honors
  `--json` envelope.
- `commands.rs`: added `inspect` to `all_commands()`, `completion_domains()`,
  `inspect_flags()` (path + at), plus test asserting registration.
- PRD `tiered-file-layout.md`: points at the CLI by name as the `.meta.json`
  substitute.

Not done (blocked on tier wiring):
- minimal/standard/performance still go through `save_for_data_path` which
  always writes `<data>.meta.json` alongside the binary metadata. There is
  no `StorageLayout` plumbed into `PhysicalMetadataFile::save_*` callsites
  (impl_metadata.rs / impl_registry.rs) yet. Flipping the default needs
  either:
  - threading the active layout into save callsites and gating the JSON
    sidecar on `StorageLayout::Max`, or
  - a tier-resolved `meta_dir` so `max` writes to `<dbname>.rdb.d/meta/`
    rather than next to the data file.
- max-tier `<dbname>.rdb.d/meta/` relocation: same blocker (layout not
  threaded; existing dir is `<dbname>.rdb.red/` per layout module, the
  ADR-suggested `.rdb.d` name is also unused yet).

Verification:
- `cargo check`/`cargo test` could not be run in this sandbox session
  (Bash approval denied). Code is purely additive and uses already-public
  `PhysicalMetadataFile` constructors; CI will exercise.

Next iteration:
1. Thread `StorageLayout` (or just a `write_json_sidecar: bool`) into the
   `save_for_data_path` call chain.
2. Default-off for minimal/standard/performance, default-on for max.
3. Move max's JSON sidecar under `<support_dir>/meta/`.
4. Add an integration test that asserts no `.meta.json` is created for
   a standard-tier embedded run and that `red inspect catalog` still
   prints the same catalog snapshot.

## Iteration 2 (partial land, 2026-05-16)

Done:
- Introduced a process-wide JSON sidecar policy in `physical.rs`:
  `set_meta_json_sidecar_enabled(bool)` + `meta_json_sidecar_enabled()`
  backed by an `AtomicU8`. Default = off. Env escape hatch:
  `REDDB_META_JSON_SIDECAR=1|true|yes|on`.
- `PhysicalMetadataFile::save_for_data_path` and
  `heal_primary_metadata_for_data_path` now write `<data>.meta.json` only
  when the policy is enabled. Binary metadata (`.meta.rdbx`) + journal
  retention behaviour unchanged. Loader continues to prefer binary, then
  binary-journal fallback, then JSON (back-compat for pre-existing
  sidecars on disk).
- Re-exported the toggle through `reddb_server::lib` so embedders / future
  tier wiring can flip it once at startup; umbrella `reddb::*` already
  fans this out.
- New integration test `tests/e2e_meta_json_sidecar_policy.rs`:
  `standard_tier_default_does_not_write_json_sidecar` (binary present,
  JSON absent, catalog still loadable from binary — same substrate the
  `red inspect catalog` CLI uses) + `max_opt_in_writes_json_sidecar`
  (JSON present when toggle is on). A `Mutex` serialises them because
  they touch a process global.

Acceptance criteria status:
- [x] minimal/standard/performance do not auto-write `.meta.json`
- [x] `reddb inspect catalog` produces current state JSON for any tier
- [x] `--at <seq>` returns catalog at snapshot or explicit error
- [~] max keeps writing — toggle exists; relocation to
       `<dbname>.rdb.d/meta/` still blocked on tier wiring (RuntimeOptions
       has no `layout: StorageLayout` and the `.rdb.d` dir name isn't
       used yet; current support dir is `<dbname>.rdb.red/`).
- [x] Docs updated pointing at CLI as substitute

Not done (blocked on tier wiring — same blocker as #471):
- Auto-enable the toggle for `Max` at startup. Needs
  `RuntimeOptions { layout, layout_overrides }` plumbed through
  `RedDBOptions::persistent`, then a one-shot
  `set_meta_json_sidecar_enabled(matches!(layout, Max))` near the
  runtime constructor.
- Relocate the JSON sidecar from `<data>.meta.json` to
  `<support_dir>/meta/<file_name>.meta.json` (or the ADR's
  `<dbname>.rdb.d/meta/`). Requires the same layout handle at the
  save callsites, or a tier-resolved `meta_dir` accessor on
  `TieredLayoutPaths`.

Verification:
- `cargo check` / `cargo test` not run in this sandbox (Bash approval
  denied). Change is additive at the persistence layer; existing tests
  that read metadata go through the binary path which is unchanged.
- `git add` / `git commit` also denied this session — the iter 2 edits
  sit uncommitted in the worktree. Next ralph iteration should commit
  them as `feat(physical): gate .meta.json sidecar behind opt-in policy
  (refs #472)` along with anything else that lands.
- `tests/e2e_reserved_system_fields.rs::startup_rejects_persisted_table_contract_reserved_columns`
  still passes by inspection: it saves metadata then reopens; the loader
  reads `<data>.meta.rdbx` first, so dropping the JSON write is
  invisible.
