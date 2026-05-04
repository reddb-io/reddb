# Move RedWire frame types and codecs into reddb-wire [AFK]

GitHub issue: reddb-io/reddb#57
Parent PRD: reddb-io/reddb#54
Blocked by: #56

Move the RedWire frame layout, header types, framing codec, and any transport-agnostic protocol vocabulary (per ADR 0001) into `reddb-wire`. Both `reddb-client` and `reddb-server` will depend on this crate later.

## Acceptance Criteria
- [ ] RedWire frame types, header layout, framing codec live in `reddb-wire`
- [ ] No engine/storage/runtime deps in `reddb-wire`
- [ ] `reddb` re-exports moved types so existing imports compile unchanged
- [ ] `cargo build --bin red` and full test suite stay green
- [ ] ADR 0001 unchanged; no protocol semantics change

## Feedback Loops (Rust)
- `cargo check`
- `cargo test`
