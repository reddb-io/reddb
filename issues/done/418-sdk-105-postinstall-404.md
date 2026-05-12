# SDK 1.0.5 postinstall 404 on red-linux-x86_64 release asset [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/418

Labels: needs-triage

GitHub issue number: #418

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Bug — release distribution

## Symptom

SDK 1.0.5's postinstall fetches `red-linux-x86_64` from `releases/download/v1.0.5/` — returns 404. The asset is not published for that tag. Users on Linux x86_64 cannot install RedDB 1.0.5 without manually overriding `REDDB_BIN`. 1.0.7 works.

## Impact

High. Anyone discovering RedDB via npm and pinning to 1.0.5 (likely or random) gets a broken install. Trust hit.

## Acceptance criteria

- [x] Publish missing `red-linux-x86_64` asset to the 1.0.5 release (or unpublish the npm package version if not feasible).
- [x] Release-publish workflow gains a check: every npm release REQUIRES the binary assets be present before npm publish proceeds; otherwise publish fails.
- [x] Postinstall produces a clear actionable error pointing to the manual-install path when an asset 404s.
- [x] Document the release-asset contract in `docs/release-runbook.md`.

## Completion notes

- Confirmed `https://github.com/reddb-io/reddb/releases/download/v1.0.5/red-linux-x86_64` resolves to a release asset.
- Added a postinstall regression test covering the actionable `ASSET_NOT_FOUND` message and wired it into the JS SDK test script.
- Existing release workflow gate and runbook coverage are asserted by `scripts/release_tooling_contract.test.mjs`.

## Verification

- `node --test drivers/js/test/postinstall.test.mjs`
- `pnpm --dir drivers/js test`
- `node --test scripts/release_tooling_contract.test.mjs`
