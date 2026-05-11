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

## Registry ownership and package names

RedDB publishes under two registry conventions:

- npm packages use the `@reddb-io/*` organization scope.
- crates.io packages use the `reddb` / `reddb-*` prefix because crates.io
  does not support npm-style organization scopes.

Run this local invariant before changing release or driver manifests:

```bash
node scripts/check-registry-names.mjs
```

### npm

The canonical public npm packages are:

- `@reddb-io/cli`
- `@reddb-io/sdk`
- `@reddb-io/client`
- `@reddb-io/client-bun`

Support helper packages publish under `@reddb-io/internal-*`. They are
not user-facing APIs, but they must be public because the CLI/SDK/client
packages depend on them at install time.

Publishing requires an npm token that can publish to the `reddb-io` org.
If local checks return `E401`, refresh the token with:

```bash
npm login
npm whoami
npm org ls reddb-io
```

### crates.io

crates.io organization ownership is represented through a GitHub team owner,
not through a registry namespace. The canonical team owner is:

```text
github:reddb-io:crates-owners
```

The GitHub team must exist in the `reddb-io` org, and crates.io must be
allowed to read GitHub org membership. The operator account needs to
reauthenticate on crates.io with GitHub `read:org` when team-owner commands
fail with an org/team permission error.

Apply the team owner to all existing crates:

```bash
bash scripts/configure-crates-owners.sh
```

Crates that do not exist yet will be reported as pending. Add the same team
immediately after their first publish:

```bash
cargo owner --add github:reddb-io:crates-owners reddb-client
cargo owner --add github:reddb-io:crates-owners reddb-wire
```

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
