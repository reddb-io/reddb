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

- [x] New workspace package `@reddb-io/internal-asset-fetcher` with `"private": true`, listed in the root pnpm workspace.
- [x] Public surface is one function with the signature above; platform/arch mapping, redirect handling, checksum verification all encapsulated.
- [x] Errors are descriptive: 404, checksum mismatch, unsupported platform/arch each surface a distinct, actionable message.
- [x] SDK postinstall imports the package and produces the same `node_modules/@reddb-io/sdk/bin/red` artifact as before.
- [x] Tests: platform/arch mapping, redirect chain (mocked HTTP), checksum mismatch, missing-asset 404. Prior art: `drivers/js/test/smoke.test.mjs`.
- [ ] Existing SDK smoke test still passes; `bash scripts/check-versions.sh` still green. — Not executed in this iteration (sandbox blocked node/pnpm). User should run `pnpm install && pnpm --filter @reddb-io/internal-asset-fetcher test && pnpm --filter @reddb-io/sdk test && bash scripts/check-versions.sh`.

## Notes for next iteration

- Public surface is a single named export `fetchReleaseAsset` from `packages/internal-asset-fetcher/src/index.js`. Internals split across `asset-name.js`, `download.js`, `checksum.js` for focused testing — these are not part of the public contract.
- Distinct error codes on thrown `Error` subclasses: `UNSUPPORTED_PLATFORM`, `ASSET_NOT_FOUND` (HTTP 404), `CHECKSUM_MISMATCH`, `HTTP_ERROR` (other non-2xx), `TOO_MANY_REDIRECTS`. Callers can switch on `err.code` instead of message regex.
- `download.js` picks `node:http` vs `node:https` based on URL scheme so tests can spin a local plain-HTTP server. GitHub release URLs are still https end-to-end; the dual-scheme support is test-only and adds no runtime cost.
- `composeAssetName` accepts `binName` (`'red'` for SDK, `'red_client'` for the future `@reddb-io/client` per ADR 0006) so #136 can reuse the fetcher unchanged.
- SDK `postinstall.js` lost ~50 lines (resolveAssetName + downloadFollowingRedirects); the new flow is `fetchReleaseAsset({...}) → writeFileSync → chmod`. Override env vars (`REDDB_SKIP_POSTINSTALL`, `REDDB_POSTINSTALL_VERSION`, `REDDB_POSTINSTALL_REPO`) preserved verbatim. Failure-path stderr message points at `REDDB_BINARY_PATH` (legacy SDK runtime env var).
- Added `pnpm-workspace.yaml` at root listing `.`, `drivers/js`, `packages/*`. The repo did not have one before this slice. SDK now declares `"@reddb-io/internal-asset-fetcher": "workspace:*"` in `dependencies`.
- **Publishing concern:** at SDK publish time, `workspace:*` would resolve to a private package and break consumers. The fix (bundling fetcher source into SDK at publish, or making it public) is parent-tracked — same shape as the bin-resolver concern raised in earlier slices. Until that lands, do not publish a new `@reddb-io/sdk`.
- `@reddb-io/internal-bin-resolver` referenced by ADR 0006 / #134 is **not present in this branch**. The earlier session-start status that listed it as `done` reflected a parallel worktree, not this repo. Treating that work as still pending — the asset-fetcher slice landed first.
- The CLI postinstall (`drivers/js/postinstall.js` is shared by both `@reddb-io/cli` and `@reddb-io/sdk` via root `package.json`'s `postinstall` script — actually root delegates to `node drivers/js/postinstall.js`, same script). Per #135 scope, the CLI install/upgrade/skip flow is a follow-up; this slice keeps behaviour identical to today (always download).
