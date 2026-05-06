# ADR 0006 — npm package matrix (cli / sdk / client) and pinned-binary rationale [AFK]

GitHub: reddb-io/reddb#132

Author `docs/adr/0006-npm-package-matrix.md` capturing the decision to ship three npm packages under `@reddb-io/` (`cli`, `sdk`, `client`) and the rules for how each acquires the underlying Rust binary on install.

The ADR must answer:

- Why three packages instead of one with conditional behaviour.
- Why `@reddb-io/sdk` and `@reddb-io/client` pin the binary inside `node_modules` (wire-format coupling — a PATH binary at the wrong version silently breaks RPC).
- Why `@reddb-io/cli` is allowed to use a global PATH binary and reconcile via version comparison.
- Runtime resolution order for SDK/client: `REDDB_BIN` / `REDDB_CLIENT_BIN` env → local `node_modules/.../bin/`. PATH **not** consulted.
- CLI postinstall decision tree: PATH absent → install; PATH older → upgrade; PATH equal/newer → skip.
- Which scenarios go to which package (server CLI, full app SDK with embedded, serverless/edge thin client).

Format mirrors the existing ADRs in `docs/adr/0004-*` and `docs/adr/0005-*`.

## Acceptance Criteria

- [ ] `docs/adr/0006-npm-package-matrix.md` exists and follows the 0004/0005 format.
- [ ] Three packages enumerated with target audience, install size budget, binary acquisition strategy.
- [ ] SDK/client runtime lookup precedence documented (env → local node_modules); explicit statement that PATH is not consulted.
- [ ] CLI postinstall version-compare flow (install / upgrade / skip) documented with rationale.
- [ ] Cross-links to the implementation slices: #133 (asset-fetcher), #134 (bin-resolver), #135 (version-compare), #136 (client package), #137 (release pipeline), #138 (docs).
- [ ] Status: `Accepted`.
