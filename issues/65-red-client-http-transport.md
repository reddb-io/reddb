# red_client: wire HTTPS/HTTP transport [AFK]

GitHub: reddb-io/reddb#65
Parent: #54

`red_client` rejects http(s):// today. Add REST/HTTPS connector to `reddb-client-internal` so red_client can hit a `red --http` listener.

## Acceptance Criteria
- [ ] http:// + https:// connect end-to-end
- [ ] Bearer + basic auth
- [ ] HTTPS validates server cert
- [ ] Cross-binary smoke test for HTTPS round-trip
- [ ] Size guard #62 stays green

## Feedback Loops (Rust)
- `cargo test -p reddb-client-internal`
- `cargo test --test cross_binary_smoke`
