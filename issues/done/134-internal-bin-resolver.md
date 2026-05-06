# Extract @reddb-io/internal-bin-resolver deep module + rewire SDK runtime [AFK]

GitHub: reddb-io/reddb#134
Parent: #132 (ADR 0006)

The SDK runtime currently locates the `red` binary via ad-hoc lookup with PATH still in the search order. ADR 0006 forbids PATH for SDK/client and prescribes `env → local node_modules/.../bin/`.

Lift the lookup into a workspace-only deep module — `@reddb-io/internal-bin-resolver` (`"private": true`) — with one pure function:

```
resolveBin({ name, packageRoot, envVar }) → string  // throws actionable error on miss
```

Rewire SDK runtime (embedded factory in particular) to call `resolveBin`. Remove the PATH fallback. Surface a clear error when missing — pointing at the env override and postinstall log.

This slice is independent of #133: the two deep modules can land in any order. Once both are merged, `@reddb-io/client` (#136) can compose them.

## Acceptance Criteria

- [x] New workspace package `@reddb-io/internal-bin-resolver` with `"private": true`, in the pnpm workspace.
- [x] Pure function `resolveBin` honours precedence env → `<packageRoot>/bin/<name>`. PATH is not consulted.
- [x] Missing-binary error names the env var, expected local path, and a one-line `pnpm install` hint.
- [x] SDK runtime (embedded factory plus any other lookup site) consumes `resolveBin`; legacy PATH-tolerant code paths removed from SDK lookup.
- [x] Unit tests: env override wins, env unset falls through to local path, missing local path raises actionable error, env pointing at non-existent file is honoured verbatim (per ADR — env is "I know what I'm doing").
- [ ] Existing SDK smoke + embedded coverage still passes — not executed in this iteration (sandbox blocked node/pnpm). User should run `pnpm install && pnpm --filter @reddb-io/internal-bin-resolver test && pnpm --filter @reddb-io/sdk test`.

## Notes for next iteration

- `pnpm-workspace.yaml` was added (root `.`, `drivers/js`, `packages/*`).
- `@reddb-io/sdk` now depends on `@reddb-io/internal-bin-resolver` via `workspace:*`. The resolver package is `"private": true` per ADR 0006. **Publishing concern:** at SDK publish time, `workspace:*` would resolve to a private package and break consumers. The fix (bundling the resolver source into SDK at publish, or making it public) is parent-tracked under #137 (release pipeline). Until #137 lands, do not publish a new `@reddb-io/sdk`.
- `REDDB_BINARY_PATH` (legacy) is still honoured by `resolveSdkBinary` when `REDDB_BIN` is unset — deprecation window per ADR. No console.warn yet.
- `cli.js` (`@reddb-io/cli`) was rewired to a separate `resolveCliBinary` helper that retains PATH fallback (per ADR — CLI is allowed to consult PATH because it *targets* PATH).
- Issue #135 (CLI postinstall) and #133 (asset-fetcher) remain independently grabbable. #136 (`@reddb-io/client`) can now compose the resolver.
