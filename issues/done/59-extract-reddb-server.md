# Extract reddb-server crate; reddb umbrella re-exports both [AFK]

GitHub issue: reddb-io/reddb#59
Parent PRD: reddb-io/reddb#54
Blocked by: #58

Create `crates/reddb-server/` and move all server-side modules: engine, storage, runtime, replication, MCP, AI, server handlers, server-side auth. Depends on `reddb-wire`. Umbrella `reddb` becomes thin: re-exports from all three crates + hosts `red` bin.

## Acceptance Criteria
- [ ] `crates/reddb-server/` is workspace member, deps `reddb-wire`
- [ ] Engine/storage/runtime/replication/MCP/AI/server handlers/server auth live in `reddb-server`
- [ ] Umbrella keeps `red` bin + re-exports public types from all three crates
- [ ] `cargo build --bin red` parity in behavior
- [ ] Full test suite (embedded smoke + integration) green
- [ ] `tests/` either keeps `use reddb::…` paths or migration is clearly documented

## Feedback Loops (Rust)
- `cargo check`
- `cargo test`
