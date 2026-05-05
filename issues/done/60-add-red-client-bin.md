# Add red_client binary with embedded-scheme rejection [AFK]

GitHub issue: reddb-io/reddb#60
Parent PRD: reddb-io/reddb#54
Blocked by: #58

Add `red_client` binary at `crates/reddb-client/src/bin/red_client.rs`. Accepts remote schemes (`red://`, `reds://`, `http(s)://`, `grpc(s)://` + query-param vocabulary). Rejects embedded schemes (`red://`, `red:///path`, `red://:memory:`) at parse time with stable distinct exit code + fixed error message pointing at `red`.

## Acceptance Criteria
- [ ] `cargo build --bin red_client` builds; links only `reddb-client` + `reddb-wire` (no engine symbols)
- [ ] Remote schemes connect successfully against running `red`
- [ ] `reds://host` defaults to RedWire-over-TLS without flags
- [ ] Embedded schemes exit with documented error + distinct stable exit code
- [ ] Both interactive REPL and one-shot command modes work
- [ ] PG wire (`?proto=pg`) rejected; documented
- [ ] Diagnostic output does not leak credentials
- [ ] Full auth vocabulary wired through

## Feedback Loops (Rust)
- `cargo check`
- `cargo test -p reddb-client`
- `cargo test`
