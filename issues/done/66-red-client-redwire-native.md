# red_client: wire RedWire native transport [AFK]

GitHub: reddb-io/reddb#66
Parent: #54

`red://` / `reds://` route through gRPC today as stopgap. Build real RedWire client using `reddb-wire::redwire` codec.

## Acceptance Criteria
- [ ] red://host:5050 round-trips Query frame end-to-end
- [ ] reds://host:5050 over TLS, validates cert
- [ ] mTLS via ?cert=&key=&ca= query params
- [ ] SCRAM-256, bearer, OAuth/JWT auth (per ADR 0001)
- [ ] Cross-binary smoke for RedWire plain + TLS
- [ ] Size guard #62 stays green

## Feedback Loops
- `cargo test -p reddb-client-internal`
- `cargo test --test cross_binary_smoke`
