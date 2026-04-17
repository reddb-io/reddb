# Todo — Ultrareview Fixes

## Phase 1: CI Unblock + Silent Regression
- [ ] **Task 1** — Fix 5 context_index unit tests (opt-in `set_collection_enabled`). `src/storage/unified/context_index.rs:714-786`. XS.
- [ ] **Task 2** — Legacy sidecar: decode missing `context_index_enabled` as `true`. `src/physical/json_codec.rs:424-427` + regression test. XS.
- [ ] **Checkpoint 1** — `cargo test` green for `context_index` + `json_codec`; `cargo check` clean; human review.

## Phase 2: Telemetry Merge Correctness
- [ ] **Task 3** — Add `_explicit` bools + `log_file_disabled` to `TelemetryConfig`; wire CLI parser. `src/telemetry/mod.rs`, `src/bin/red.rs:1580-1600`. S.
- [ ] **Task 4** — Rewrite `merge_telemetry_with_config` to gate on `_explicit`; `--no-log-file` beats config; add merge tests. `src/service_cli.rs:823-884`. S.
- [ ] **Checkpoint 2** — Merge priority verified; `--no-log-file` honoured.

## Phase 3: TelemetryGuard Lifetime
- [ ] **Task 5** — Return guard from `build_runtime_and_auth_store`; bind in all 5 server runners. `src/service_cli.rs:556,736,745-755,904,926,964`. S.
- [ ] **Checkpoint 3 (final)** — `cargo build --release` clean; rotating file logs written; `--no-log-file` suppresses; `red.logging.dir` honoured.

## Out of scope (follow-ups)
- `ALTER TABLE ... SET context_index = {true,false}` escape hatch
- Startup `tracing::warn!` listing legacy contracts auto-defaulted to enabled
