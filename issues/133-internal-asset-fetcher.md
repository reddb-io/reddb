# Extract @reddb-io/internal-asset-fetcher deep module + rewire SDK postinstall [AFK]

GitHub: reddb-io/reddb#133
Parent: #132 (ADR 0006)

Today `drivers/js/postinstall.js` inlines all logic that downloads the matching `red` binary from a GitHub release: platform/arch detection, multi-redirect HTTP, asset-name composition, and (where present) checksum verification.

Lift that into a workspace-only npm package — `@reddb-io/internal-asset-fetcher` (`"private": true`) — exposing one function:

```
fetchReleaseAsset({ repo, tag, platform, arch, binName, sha256? }) → Buffer
```

Rewire the existing SDK postinstall to consume the new package. End-user behaviour must be byte-identical: `pnpm add @reddb-io/sdk` still pulls the matching `red` binary into `node_modules/@reddb-io/sdk/bin/red`. The `@reddb-io/cli` postinstall stays untouched in this slice (slice #135 rewires it).

## Acceptance Criteria

- [ ] New workspace package `@reddb-io/internal-asset-fetcher` with `"private": true`, listed in the root pnpm workspace.
- [ ] Public surface is one function with the signature above; platform/arch mapping, redirect handling, checksum verification all encapsulated.
- [ ] Errors are descriptive: 404, checksum mismatch, unsupported platform/arch each surface a distinct, actionable message.
- [ ] SDK postinstall imports the package and produces the same `node_modules/@reddb-io/sdk/bin/red` artifact as before.
- [ ] Tests: platform/arch mapping, redirect chain (mocked HTTP), checksum mismatch, missing-asset 404. Prior art: `drivers/js/test/smoke.test.mjs`.
- [ ] Existing SDK smoke test still passes; `bash scripts/check-versions.sh` still green.
