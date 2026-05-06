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

- [ ] New workspace package `@reddb-io/internal-version-compare` with `"private": true`.
- [ ] `compareInstalled` parses semver from `red --version`, handles missing binary, exec failure, malformed output, prerelease suffixes.
- [ ] CLI postinstall consumes the module; three branches each emit a distinct one-line log message.
- [ ] `REDDB_SKIP_POSTINSTALL=1` and `REDDB_BIN` overrides remain honoured.
- [ ] Unit tests: equal → skip, package newer → upgrade, package older → skip, malformed PATH version → install (warn), exec failure → install (warn), prerelease ordering.
- [ ] Manual check: `npm i -g @reddb-io/cli` against older PATH `red` upgrades; against newer PATH `red` skips.
- [ ] No regression in existing CLI smoke flow.
