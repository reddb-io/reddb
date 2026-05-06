# Add @reddb-io/internal-version-compare + wire CLI postinstall (install / upgrade / skip) [AFK]

GitHub: reddb-io/reddb#135
Parent: #132 (ADR 0006)

The current `@reddb-io/cli` postinstall always downloads, even when the user has a newer `red` on PATH. ADR 0006 prescribes a different flow:

- **install** — PATH binary absent → fetch and place into the global location.
- **upgrade** — PATH binary present but older than the package version → fetch and overwrite with a one-line log.
- **skip** — PATH binary equal or newer → log and exit.

Build the comparison as a workspace-only deep module — `@reddb-io/internal-version-compare` (`"private": true`) — with exec injectable:

```
compareInstalled({ packageVersion, exec }) → { action: 'install' | 'upgrade' | 'skip', reason: string }
```

Wire CLI postinstall to call this module and act on its verdict. Asymmetry vs SDK/client is intentional and documented in the ADR.

## Acceptance Criteria

- [x] New workspace package `@reddb-io/internal-version-compare` with `"private": true`.
- [x] `compareInstalled` parses semver from `red --version`, handles missing binary, exec failure, malformed output, prerelease suffixes.
- [x] CLI postinstall consumes the module; three branches each emit a distinct one-line log message.
- [x] `REDDB_SKIP_POSTINSTALL=1` and `REDDB_BIN` overrides remain honoured.
- [x] Unit tests: equal → skip, package newer → upgrade, package older → skip, malformed PATH version → install (warn), exec failure → install (warn), prerelease ordering.
- [ ] Manual check: `npm i -g @reddb-io/cli` against older PATH `red` upgrades; against newer PATH `red` skips. — Sandbox blocks node/pnpm in this iteration. User should run `pnpm install && pnpm --filter @reddb-io/internal-version-compare test`, then validate against a real-PATH scenario.
- [ ] No regression in existing CLI smoke flow. — Same sandbox limitation; `bash scripts/check-versions.sh` and `pnpm test` not executed here.

## Notes for next iteration

- Public surface is `compareInstalled` plus two helpers (`parseVersion`, `compareSemver`) exported only for the unit tests. Internals are 100 % stdlib-free — no semver dep — to keep the deep module zero-cost in the postinstall hot path.
- Verdict shape matches ADR 0007 §"CLI postinstall" verbatim, including the `skip` branches for `equal` and `PATH newer`. Reasons are user-facing: each one names both the installed version and the package version where applicable, so the postinstall log is self-explanatory.
- Custom semver compare implements §11 of semver 2.0.0 (numeric < alphanumeric in prerelease identifiers, no-prerelease > prerelease at equal core, longer prerelease tail wins on equal prefix). Build metadata (`+sha…`) is parsed but ignored per spec.
- New `drivers/js/cli-postinstall.js` replaces `drivers/js/postinstall.js` as the root `@reddb-io/cli` postinstall entry. The SDK keeps its own `drivers/js/postinstall.js` unchanged — that file still always downloads, which is correct for the wire-coupled SDK case.
- Root `package.json` now declares `@reddb-io/internal-asset-fetcher` and `@reddb-io/internal-version-compare` as `workspace:*` deps. Same publishing concern as the SDK: at publish time `workspace:*` would resolve to a private package and break consumers. Bundling internals into the published artifact (or making the internal packages public) is parent-tracked under ADR 0006/0007. Until that lands, do not publish a new `@reddb-io/cli`.
- `REDDB_BIN` and `REDDB_SKIP_POSTINSTALL=1` short-circuit before `compareInstalled` runs — the env overrides are unconditional, never fall through to the version compare.
- `exec` callback uses `execSync('red --version', { stdio: ['ignore','pipe','ignore'] })` so a missing binary throws ENOENT, which `compareInstalled` translates into `action='install'` with a descriptive reason.
- The four log lines are distinguishable by an `install`/`upgrade`/`skip` verb in the first sentence, suitable for grep in CI logs.
