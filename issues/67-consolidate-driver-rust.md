# Consolidate drivers/rust + reddb-client-internal [HITL]

GitHub: reddb-io/reddb#67
Parent: #54

Two crates overlap: `drivers/rust` (published `reddb-client`) + `crates/reddb-client` (`reddb-client-internal`, `publish=false`). Merge into one published `reddb-client`.

HITL: needs design decision on public API preservation strategy.

## Acceptance Criteria
- [ ] One canonical `reddb-client` crate
- [ ] `reddb-wire::conn_string::parse` is only parser in repo
- [ ] Public API preserved or migration documented
- [ ] `red_client` bin in consolidated crate
- [ ] CI / release.yml / sync-version.js updated
- [ ] Driver tests pass

## Feedback Loops
- `cargo test -p reddb-client`
- `bash scripts/check-versions.sh`
