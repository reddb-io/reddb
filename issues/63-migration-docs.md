# Migration guide and per-crate READMEs for the workspace split [AFK]

GitHub issue: reddb-io/reddb#63
Parent PRD: reddb-io/reddb#54
Blocked by: #59

Document new crate boundaries. Each new crate (`reddb-wire`, `reddb-client`, `reddb-server`) gets `README.md` linking ADR 0001 + connection-strings doc + intended audience. Migration guide under `docs/` lists representative old → new import paths + umbrella re-exports.

## Acceptance Criteria
- [ ] `crates/reddb-wire/README.md`, `crates/reddb-client/README.md`, `crates/reddb-server/README.md` exist
- [ ] Each links ADR 0001 + connection-strings doc + states audience
- [ ] Migration guide under `docs/` with old → new import paths
- [ ] Umbrella `reddb` README links migration guide
- [ ] Docs only; no code changes

## Feedback Loops
- Lint markdown if available; otherwise visual review
