# Extract reddb-client crate (lib only) with REPL and connector [AFK]

GitHub issue: reddb-io/reddb#58
Parent PRD: reddb-io/reddb#54
Blocked by: #57

Create `crates/reddb-client/` as library only (no bin yet). Move multi-protocol connector (RedWire TCP/TLS, HTTPS REST, gRPC plain/TLS) and existing REPL. Implement full auth vocabulary (anonymous, bearer, basic, SCRAM-SHA-256, OAuth-JWT, mTLS). Depends only on `reddb-wire`.

## Acceptance Criteria
- [ ] `crates/reddb-client/` is workspace member, deps: `reddb-wire` + transport/TLS/auth third-party only
- [ ] Multi-protocol connector + REPL live in `reddb-client`
- [ ] Full auth vocabulary supported
- [ ] TLS defaults validate certs; no implicit "skip verify"
- [ ] `reddb` re-exports connector + REPL; existing call sites unchanged
- [ ] `cargo build --bin red` + full test suite green
- [ ] No `red_client` binary yet

## Feedback Loops (Rust)
- `cargo check`
- `cargo test`
