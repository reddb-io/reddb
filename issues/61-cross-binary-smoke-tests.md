# Cross-binary smoke tests: red_client against running red [AFK]

GitHub issue: reddb-io/reddb#61
Parent PRD: reddb-io/reddb#54
Blocked by: #60

CI integration tests booting full `red` binary and connecting `red_client` over each remote transport: RedWire plain, RedWire TLS, HTTPS, gRPC plain, gRPC TLS. Each test runs trivial round-trip query end-to-end. Follows prior art: `tests/redwire_oauth_e2e.rs`, `tests/grpc_*_smoke.rs`, `tests/http_*_smoke.rs`.

## Acceptance Criteria
- [ ] Smoke tests boot `red` and run `red_client` against it
- [ ] Cover RedWire plain, RedWire TLS, HTTPS, gRPC plain, gRPC TLS
- [ ] Each runs at least one round-trip query
- [ ] Run in CI on every PR
- [ ] No flaky timeouts hiding regressions

## Feedback Loops (Rust)
- `cargo test --test '*smoke*'`
- `cargo test`
