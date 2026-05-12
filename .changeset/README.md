# Changesets

This folder is managed by [Changesets](https://github.com/changesets/changesets) — every PR that needs a release entry drops a markdown file here describing the bump and what changed. The CI Release PR aggregates all changesets into a single version bump + tag + GitHub Release + npm/crates/PyPI publish.

## Daily workflow for contributors

When your PR touches anything user-visible (driver behaviour, server behaviour, schema, public API, etc.), run:

```bash
pnpm changeset
```

The CLI asks:

1. **Which packages changed?** Pick from `@reddb-io/{cli,sdk,client,client-bun}`. These four are **locked together** (`fixed` in `config.json`) — touching one bumps all four to the same version. That keeps the npm SDK in sync with the engine binary.
2. **What kind of bump?** `patch` for fixes, `minor` for back-compat features, `major` for breaking changes. Lock-step group all bump to the highest level chosen.
3. **Summary.** One line — this lands in `CHANGELOG.md` and the GitHub Release notes via `@changesets/changelog-github`.

This creates `.changeset/<random-slug>.md`. Commit it with your code change in the same PR.

If your PR is internal-only (CI, refactor, docs), skip it. The Release PR will still ship without a new changeset; it just won't have an entry for your work.

## What the CI does

Pushes to `main` trigger `.github/workflows/changesets.yml`:

- **If there are pending changesets**, the action opens or updates a single "Version Packages" PR. The PR shows the pending bumps and the auto-generated changelog.
- **When that PR is merged**, the action runs `pnpm release:version` — which calls `changeset version` (bumps the four npm packages, writes CHANGELOG.md) followed by `scripts/sync-version.js` (propagates the new version into `Cargo.toml`, `crates/**/Cargo.toml`, `drivers/python/{Cargo,pyproject}.toml`, `packages/internal-*/package.json`, and `drivers/bun/package.json`). It then commits, tags `v<version>`, and pushes.
- The pushed tag triggers `.github/workflows/release.yml`, which builds all platform binaries, creates the GitHub Release with the assets, and only **after** that succeeds publishes to npm, crates.io, PyPI, and GHCR. The four `publish-*` jobs in `release.yml` have `needs: [plan, publish-github]` — so npm never has a version that lacks a GitHub Release.

## What changed vs the old flow

We used to run `pnpm version patch` locally. That tagged immediately and pushed, opening a window where `package.json` on `main` had a version with no corresponding GitHub Release — the `postinstall` of `@reddb-io/sdk` would `404` until the release workflow finished. Changesets eliminates that: the version bump and the release happen atomically in CI.

## Reference

- Tool: <https://github.com/changesets/changesets>
- Config: [`config.json`](./config.json) (`fixed` array enforces the four-package lock-step)
- Release runbook: [`../docs/release-runbook.md`](../docs/release-runbook.md)
