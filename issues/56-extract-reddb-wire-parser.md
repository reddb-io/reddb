# Extract reddb-wire crate with connection-string parser [AFK]

GitHub issue: reddb-io/reddb#56
Parent PRD: reddb-io/reddb#54
Blocked by: #55

Create the `reddb-wire` workspace crate at `crates/reddb-wire/` and move the connection-string parser into it as the public deep module. Parser turns documented URLs (`red://`, `reds://`, `http(s)://`, `grpc(s)://`, plus all documented embedded forms) into a normalized `ConnectionTarget` value.

## Acceptance Criteria
- [ ] `crates/reddb-wire/` exists, no engine deps
- [ ] Parser exposes typed `ConnectionTarget` (or equivalent)
- [ ] Table-driven tests cover every scheme/transport/query parameter from `docs/clients/connection-strings.md` + invalid inputs
- [ ] `reddb` umbrella re-exports parser; existing call sites compile unchanged
- [ ] `cargo build --bin red` and full test suite stay green

## Feedback Loops (Rust)
- `cargo check`
- `cargo test -p reddb-wire`
- `cargo test`
