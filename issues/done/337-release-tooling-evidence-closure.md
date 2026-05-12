# Release tooling evidence closure [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/337

Labels: enhancement

GitHub issue number: #337

## Parent

#333 (https://github.com/reddb-io/reddb/issues/333)

## What to build

Close evidence gaps for red_client size guard, red_client container distribution, and the 2026-05-06 nightly DR drill failure. The slice should verify the public Makefile/workflow/artifact contracts.

Covers: #62, #68, #116

User stories covered: 13, 14

## Acceptance criteria

- [x] red_client binary-size guard is evidenced by a runnable CI or local check and a documented threshold.
- [x] red_client container image strategy is evidenced by release workflow/container configuration and smoke behavior.
- [x] Nightly DR drill failure #116 is tied to the current script/workflow fix and a runnable drill command.
- [x] The evidence report no longer marks #62, #68, or #116 as partial without a final disposition.

## Blocked by

- #334 (https://github.com/reddb-io/reddb/issues/334)

## Closure notes

- Added `scripts/release_tooling_contract.test.mjs` to verify the public release-tooling contracts for the `red_client` size guard, thin-client container, and nightly DR drill.
- Fixed `Dockerfile.client` to build the actual `reddb-client` package with `--no-default-features`.
- Added explicit evidence ledger dispositions for #62, #68, and #116 and regenerated both report JSON artifacts.

## Verification

- `node --test scripts/release_tooling_contract.test.mjs`
- `node --test scripts/issue_code_evidence_report.test.mjs`
- `node scripts/issue_code_evidence_report.js /tmp/reddb_issues_raw.json reports`
