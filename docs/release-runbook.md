# Release runbook

Operator-facing runbook for tagging + post-publish maintenance steps that
sit outside `.github/workflows/release.yml`.

## Cutting a release

1. Land all PRs targeting the release on `main`.
2. From `main`, run `pnpm version <patch|minor|major>` (or `pnpm version 1.0.0`
   for a pinned bump). The npm `version` lifecycle hook calls
   `scripts/sync-version.js`, which propagates the new version to every
   lock-stepped manifest (`Cargo.toml`, `crates/**/Cargo.toml`,
   `drivers/js/package.json`, `drivers/js-client/package.json`,
   `drivers/python/Cargo.toml`, `drivers/python/pyproject.toml`) and stages
   the result.
3. Verify with `bash scripts/check-versions.sh` — all lock-stepped targets
   must report the same version.
4. `git push --follow-tags`. The pushed tag triggers
   `.github/workflows/release.yml`, which runs the `publish-js-cli`,
   `publish-js-driver`, `publish-js-client`, and crates.io publish jobs.

## Deprecating legacy npm packages

Some packages were published under names that pre-date the `@reddb-io/*`
migration. After 1.0.0 ships, mark them deprecated so new users land on
the canonical packages.

`scripts/deprecate-legacy-npm.sh` deprecates the legacy `reddb-cli` package,
pointing users at `@reddb-io/cli`. It is **operator-triggered only** — not
wired into `release.yml`, because a CI-driven deprecate-on-every-release
loop is too easy to misfire.

Run once, after `@reddb-io/cli` 1.0.0 has been published:

```bash
# logged in as a maintainer of `reddb-cli`
bash scripts/deprecate-legacy-npm.sh
```

The script is idempotent — re-running just rewrites the same deprecation
message.

To undeprecate:

```bash
npm deprecate reddb-cli@"<all-versions>" ""
```

### Out of scope

- The unscoped `reddb` name on npm is owned by an unrelated upstream
  package. We can not deprecate it.
- `drivers/python-asyncio` and `charts/reddb` follow independent version
  policies and are not touched by the lock-step bump or this runbook.
