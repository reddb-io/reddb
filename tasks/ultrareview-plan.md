# Implementation Plan: Ultrareview Findings Fix (main branch)

## Overview

Fix four bugs surfaced by `/ultrareview`. All live in Phase 6 logging / context-index opt-in work. Two are silent production regressions (file logs dropped, legacy tables lose context index), one is a broken CLI override contract (`--no-log-file`), one breaks CI (`context_index` unit tests).

## Architecture Decisions

- **Bug 4 (legacy sidecar default)**: decode missing `context_index_enabled` key as `true`, not `false`. Preserves pre-PR "enabled unless opt-out" semantics on upgrade. Cheaper than a schema-version migration; strictly safer than silent disable. New contracts keep writing the explicit bool, so after one round-trip old sidecars stay honest to user intent.
- **Bug 1 (telemetry merge)**: replace `Option<T>` / equality-to-default tri-state with explicit `explicit: bool` flags on the CLI-built `TelemetryConfig` + a separate `log_file_disabled: bool`. Minimal shape, avoids enum churn. Merge gates read the explicit flag, not field equality.
- **Bug 2 (TelemetryGuard dropped)**: thread `Option<TelemetryGuard>` out of `build_runtime_and_auth_store` (rename return tuple) rather than duplicate call sites. Each server runner binds `_telemetry_guard` to a local for full server lifetime.
- **Bug 3 (context_index tests)**: surgical test-only fix. Add `set_collection_enabled(..., true)` at top of each affected test.

## Dependency Graph

```
Bug 3 (tests only) ── independent
Bug 4 (json_codec)  ── independent, but should land before Bug 1/2 ship
Bug 1 (telemetry merge)
   │
   └── touches TelemetryConfig shape
           │
           └── Bug 2 (guard lifetime) reads same struct, does not conflict
```

Bug 1 and Bug 2 both touch `service_cli.rs`; land Bug 1 first so Bug 2 rebases on the final merge signature.

## Task List

### Phase 1: CI Unblock + Silent Regression

#### Task 1: Fix 5 context_index unit tests (Bug 3)

**Description:** Add `index.set_collection_enabled(coll, true)` at top of each of the 5 in-file tests that construct `ContextIndex::new()` and call `index_entity`.

**Acceptance criteria:**
- [ ] `test_index_and_search`, `test_field_search`, `test_remove_entity`, `test_collection_filtering` (opts in both `col_a` and `col_b`), `test_stats` all opt the collection in before indexing
- [ ] No production code touched

**Verification:**
- [ ] `cargo test -p reddb storage::unified::context_index` — all 5 pass
- [ ] `cargo check` clean

**Dependencies:** None.

**Files likely touched:** `src/storage/unified/context_index.rs` (lines 714–786)

**Estimated scope:** XS

---

#### Task 2: Preserve context index on upgrade for legacy sidecars (Bug 4)

**Description:** Change `collection_contract_from_json` to default missing `context_index_enabled` key to `true`. Pre-PR DBs had context indexing on-by-default; the new opt-in must not silently disable existing tables.

**Acceptance criteria:**
- [ ] Missing `context_index_enabled` key decodes as `true`
- [ ] Present `false` still decodes as `false` (explicit opt-out respected)
- [ ] Present `true` still decodes as `true`
- [ ] New regression test in `src/physical/json_codec.rs` tests module covering the missing-key case

**Verification:**
- [ ] `cargo test -p reddb physical::json_codec` passes
- [ ] `cargo check` clean
- [ ] Manual: hand-craft a sidecar JSON without the key, confirm `SEARCH CONTEXT` returns rows on an inserted entity

**Dependencies:** None.

**Files likely touched:**
- `src/physical/json_codec.rs` (lines 424–427 + new test)

**Estimated scope:** XS

---

### Checkpoint: Phase 1
- [ ] `cargo test` green for `context_index` and `json_codec`
- [ ] `cargo check` clean
- [ ] Human review before Phase 2

---

### Phase 2: Telemetry Correctness

#### Task 3: Add `explicit`/`disabled` flags to TelemetryConfig (Bug 1, foundation)

**Description:** Extend `TelemetryConfig` (in `src/telemetry/mod.rs`) with non-serialised booleans tracking which fields the operator explicitly set via CLI: `level_explicit`, `format_explicit`, `rotation_keep_days_explicit`, `file_prefix_explicit`, `log_dir_explicit`, `log_file_disabled`. Default all to `false`. `default_telemetry_for_path` leaves them `false` (its outputs are implicit defaults). CLI parser in `src/bin/red.rs` sets matching `_explicit` to `true` per flag; `--no-log-file` sets `log_file_disabled = true` + `log_dir = None`.

**Acceptance criteria:**
- [ ] `TelemetryConfig` gains 6 bool flags, default `false`
- [ ] CLI parser in `src/bin/red.rs` sets `_explicit` flags only when the user passed the flag
- [ ] `--no-log-file` sets both `log_dir = None` and `log_file_disabled = true`
- [ ] Flags are not persisted/serialised anywhere (per-invocation intent)

**Verification:**
- [ ] `cargo check` clean
- [ ] Unit test verifies flag parser sets `_explicit` correctly for each flag

**Dependencies:** None (pairs with Task 4).

**Files likely touched:**
- `src/telemetry/mod.rs`
- `src/bin/red.rs` (around 1580–1600)

**Estimated scope:** S

---

#### Task 4: Rewrite `merge_telemetry_with_config` to use explicit flags (Bug 1)

**Description:** Replace the five `if cli.x == default.x` gates at `src/service_cli.rs:833–881` with `if !cli.x_explicit`. For `log_dir`, gate on `!cli.log_dir_explicit && !cli.log_file_disabled` so `--no-log-file` is never overridden by `red.logging.dir`. Delete the stale "comparison-against-default" comment block (lines 815–822).

**Acceptance criteria:**
- [ ] `red.logging.dir` promoted on persistent servers when `--log-dir` absent
- [ ] `red.logging.format` promoted on non-TTY when `--log-format` absent
- [ ] `red.logging.level` / `keep_days` / `file_prefix` merge only when flag absent
- [ ] `--no-log-file` wins over `red.logging.dir` in red_config
- [ ] New unit tests: (a) config-only `dir` promoted, (b) CLI `--log-dir` wins over config, (c) `--no-log-file` beats config `dir`, (d) non-TTY format path honours config override

**Verification:**
- [ ] `cargo test` covers new merge tests
- [ ] `cargo check` clean
- [ ] Manual smoke: `red server --path /tmp/x.rdb --no-log-file` with `red.logging.dir` persisted — no `logs/` entries written

**Dependencies:** Task 3.

**Files likely touched:**
- `src/service_cli.rs` (lines 120–144, 823–884, new `#[cfg(test)] mod tests`)

**Estimated scope:** S

---

### Checkpoint: Phase 2
- [ ] `cargo test` green
- [ ] Merge-priority matrix verified: explicit flag > red_config > default
- [ ] `--no-log-file` kill-switch honoured

---

### Phase 3: File Logging Lifetime

#### Task 5: Keep TelemetryGuard alive for server lifetime (Bug 2)

**Description:** Change `build_runtime_and_auth_store` to return `(RedDBRuntime, Arc<AuthStore>, Option<TelemetryGuard>)`. Update all 5 server runners (`run_routed_server` 556, `run_wire_only_server` 736, `run_http_server` 904, `run_grpc_server` 926, `run_dual_server` 964) to bind the guard to `_telemetry_guard` at the top so it drops only after `.serve()` returns. Remove the misleading doc comment at 748–753.

**Acceptance criteria:**
- [ ] Wrapper returns the guard (not discards)
- [ ] All 5 server runners hold the guard as a local for full scope
- [ ] Doc comment on `build_runtime_with_telemetry` (761–765) no longer contradicted by caller behaviour
- [ ] `cargo check` clean everywhere

**Verification:**
- [ ] `cargo build --release` clean
- [ ] Manual smoke: start HTTP server with `--path /tmp/x.rdb`, issue a request, stop, confirm `logs/reddb.log.<date>` contains the request log line
- [ ] `cargo test` unchanged green

**Dependencies:** Task 4 (avoid conflicting edits in service_cli.rs).

**Files likely touched:**
- `src/service_cli.rs` (lines 556, 736, 745–755, 904, 926, 964)

**Estimated scope:** S

---

### Checkpoint: Phase 3 — Complete
- [ ] All 4 bugs closed
- [ ] `cargo test` green
- [ ] `cargo build --release` clean
- [ ] Manual smoke: HTTP server writes rotating file logs; `--no-log-file` suppresses them; `red.logging.dir` honoured when no flag
- [ ] Ready for review / commit

## Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Task 2 (`unwrap_or(true)`) re-enables index for tables operators intentionally opted out of pre-PR | Low — pre-PR had no per-table opt-out, only env var | Acceptable; matches doc-stated "preserve pre-PR default" |
| Task 3 flag-threading touches CLI parser — risk of breaking other flags | Medium | Narrow edits to 6 telemetry flags; add parser unit test |
| Task 5 changes `fn` signature | Low — crate-internal `fn`, 5 call sites all in `service_cli.rs` | grep-confirmed |
| Merge-logic regression masked by TTY in dev | Medium | Task 4 tests must exercise non-TTY format branch |

## Open Questions

- Should `ALTER TABLE ... SET context_index = {true,false}` be added as follow-up? (Out of scope; operator escape hatch but not required to close 4 bugs.)
- Emit `tracing::warn!` listing legacy contracts auto-defaulted to enabled on first open post-upgrade? (Nice-to-have; skipped to keep Task 2 XS.)
