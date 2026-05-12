# Release runbook

Operator-facing runbook for cutting a release and post-publish
maintenance.

## Cutting a release (Changesets flow)

> The previous `pnpm version <patch|minor|major>` flow ran the bump
> **locally** before CI built anything, which opened a window where
> `package.json` on `main` had a version that no GitHub Release covered.
> The `postinstall` hook in `@reddb-io/sdk` would `404` in that window.
> The Changesets flow described below eliminates that race: the version
> bump and the release tag are produced atomically by CI.

### Contributors: drop a changeset on every release-worthy PR

```bash
pnpm changeset
```

Pick the affected packages (the four public npm packages are locked
together, so picking one bumps all four), choose `patch|minor|major`,
and write a one-line summary. Commit the generated
`.changeset/<slug>.md` alongside your code change. See
[`.changeset/README.md`](../../.changeset/README.md) for details.

### Operator: ship the release

1. Land all release-worthy PRs (each carrying its own changeset) on
   `main`.
2. Wait for `.github/workflows/changesets.yml` to open or update the
   "Version Packages" Release PR. The PR description shows the
   aggregated changelog and the bump it intends to apply.
3. Review the PR. If you want to defer something, drop the relevant
   `.changeset/<slug>.md` from a follow-up commit on `main`; the bot
   will refresh the PR.
4. **Merge the Release PR.** That re-fires the changesets workflow,
   which now sees zero pending changesets and therefore:
   - runs `pnpm release:version` — `changeset version` bumps the four
     npm packages and writes `CHANGELOG.md`, then
     `node scripts/sync-version.js` propagates the new version into
     every lock-stepped manifest (`Cargo.toml`, `crates/**/Cargo.toml`,
     `drivers/python/{Cargo,pyproject}.toml`,
     `packages/internal-*/package.json`, `drivers/bun/package.json`).
   - commits the version bump with author `github-actions[bot]`.
   - runs [`scripts/changesets-tag-release.sh`](../../scripts/changesets-tag-release.sh),
     which creates and pushes `v<version>`.
5. The pushed tag triggers `.github/workflows/release.yml`, which
   builds every platform binary, creates the GitHub Release with the
   assets attached, and only **after** that publishes to npm /
   crates.io / PyPI / GHCR.

There is no step at which an `@reddb-io/sdk@X.Y.Z` exists on npm
without a corresponding GitHub Release containing `red-<plat>-<arch>`
for the supported platforms.

### Manual override (skip Changesets)

If the Changesets bot is unavailable or you need an emergency cut, you
can still tag manually:

```bash
git checkout main && git pull
# bump versions yourself — `release:version` works even outside CI as
# long as there's at least one changeset under `.changeset/`.
pnpm changeset && pnpm release:version
git add -A && git commit -m "chore(release): version packages (manual)"
VERSION="$(node -e 'process.stdout.write(require(`./package.json`).version)')"
git tag -a "v$VERSION" -m "Release v$VERSION"
git push origin main "v$VERSION"
```

The pushed tag still triggers `release.yml` end-to-end.

### Verifying lock-step

```bash
bash scripts/check-versions.sh
```

All lock-stepped targets must report the same version.

## Registry ownership and package names

RedDB publishes under two registry conventions:

- npm packages use the `@reddb-io/*` organization scope.
- crates.io packages use the `reddb` / `reddb-*` prefix because crates.io
  does not support npm-style organization scopes.
- container images publish to GHCR under the `reddb-io` GitHub organization.

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
cargo owner --add github:reddb-io:crates-owners reddb-io-client
cargo owner --add github:reddb-io:crates-owners reddb-io-wire
```

### Containers

GitHub Actions publishes container images with the repository `GITHUB_TOKEN`.
No Docker Hub credentials are required by the release workflow.

Canonical images:

- `ghcr.io/reddb-io/reddb`
- `ghcr.io/reddb-io/reddb-client`

Docker Hub mirroring is intentionally disabled until a `reddb-io` Docker Hub
namespace exists and the project explicitly chooses to mirror there.

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

## Publish state across registries

The docs in `docs/clients/drivers/*.md` advertise install commands for
every supported language. Several of those registry coordinates are
**not yet published** — the package name is reserved but no version has
shipped. Track each registry below; until a row turns green, the
corresponding docs page should be paired with a "build from source"
fallback.

Probe the current state at any time:

```bash
bash scripts/check-registry-names.mjs   # local invariants
node -e "fetch('https://crates.io/api/v1/crates/reddb-io').then(r=>r.json()).then(j=>console.log(j.crate&&j.crate.max_version||'NONE'))"
curl -fsSL https://pypi.org/pypi/reddb/json | jq -r .info.version
curl -fsSL https://pypi.org/pypi/reddb-asyncio/json | jq -r .info.version
curl -fsSL https://repo.packagist.org/p2/reddb-io/reddb.json | jq -r '.packages["reddb-io/reddb"][0].version'
curl -fsSL https://pub.dev/api/packages/reddb | jq -r .latest.version
curl -fsSL https://proxy.golang.org/github.com/reddb-io/reddb-go/@latest
```

| Registry      | Coordinate                                | Driver doc                                | Status today        |
|---------------|-------------------------------------------|-------------------------------------------|---------------------|
| npm           | `@reddb-io/{cli,sdk,client,client-bun}`   | [JS/TS][jsguide], [Bun][bun]              | Published           |
| crates.io     | `reddb-io`, `reddb-io-client`, `reddb-io-server`, `reddb-io-wire`, `reddb-io-grpc-proto`, `reddb-io-client-connector` | [Rust][rust], [Embedded][emb] | Pending first publish |
| PyPI          | `reddb`                                   | [Python (PyO3)][py]                       | Pending first publish |
| PyPI          | `reddb-asyncio`                           | [Python asyncio][pyasy]                   | Pending first publish |
| Packagist     | `reddb-io/reddb`                          | [PHP][php]                                | Pending first publish |
| pub.dev       | `reddb`                                   | [Dart][dart]                              | Pending first publish |
| Go proxy      | `github.com/reddb-io/reddb-go`            | [Go][go]                                  | Pending module tag  |
| GHCR          | `ghcr.io/reddb-io/{reddb,reddb-client}`   | [Docker][docker]                          | Published           |

[jsguide]: /guides/javascript-typescript-driver.md
[bun]: /clients/drivers/bun.md
[rust]: /clients/drivers/rust.md
[emb]: /api/embedded.md
[py]: /clients/drivers/python.md
[pyasy]: /clients/drivers/python-asyncio.md
[php]: /clients/drivers/php.md
[dart]: /clients/drivers/dart.md
[go]: /clients/drivers/go.md
[docker]: /getting-started/docker.md

### First-time publish steps per registry

These are run **once per registry** by a maintainer with credentials.
After the first publish, every release pushes a new version on top
through `.github/workflows/release.yml`.

**crates.io.** All six workspace crates need to land in dependency
order on the first publish; subsequent publishes are handled by the
release workflow. From a clean checkout at the release tag:

```bash
cargo login                                              # one-time
cargo publish -p reddb-io-grpc-proto
cargo publish -p reddb-io-wire
cargo publish -p reddb-io-client-connector
cargo publish -p reddb-io-server
cargo publish -p reddb-io-client
cargo publish -p reddb-io
bash scripts/configure-crates-owners.sh                  # set team owner
```

**PyPI (`reddb`).** Built by the existing PyPI wheel job in
`release.yml` (matrix at line ~688). The `maturin` action needs a
`PYPI_API_TOKEN` repo secret. First publish:

```bash
gh secret set PYPI_API_TOKEN --body "$(pass show pypi/reddb-token)"
gh workflow run release.yml -f tag=$(cat package.json | jq -r .version)
```

**PyPI (`reddb-asyncio`).** Independent versioning (per the section
above). Publishes from `drivers/python-asyncio/` via its own
`pyproject.toml`:

```bash
cd drivers/python-asyncio
python -m build
twine upload dist/*
```

**Packagist (`reddb-io/reddb`).** Register the GitHub repo path with
Packagist once (web UI: <https://packagist.org/packages/submit>),
pointing at `https://github.com/reddb-io/reddb` with a
`composer.json` discovery hint of `drivers/php/composer.json`.
Subsequent versions tag automatically when the GitHub webhook fires.

**pub.dev (`reddb`).** First publish from `drivers/dart/`:

```bash
cd drivers/dart
dart pub login
dart pub publish --dry-run
dart pub publish
```

**Go module (`github.com/reddb-io/reddb-go`).** The Go ecosystem
discovers modules by tag on the canonical repo path. Either:

- promote `drivers/go/` to its own repo at `reddb-io/reddb-go` and
  tag `v1.x.y` there, **or**
- use `module github.com/reddb-io/reddb/drivers/go` and tag the
  monorepo with `drivers/go/v1.x.y` (Go's monorepo convention).

ADR pending — see the open driver-distribution discussion before
flipping the switch.

**GHCR.** Already published; no first-time step.

> When a registry flips from "pending" to "published", come back to
> this table and mark the row, and remove any "build from source"
> caveats from the corresponding `docs/clients/drivers/<lang>.md`.

## macOS x86_64 binary

Before issue #404 (this runbook entry), `red-macos-x86_64` and
`red_client-macos-x86_64` were **not produced** — only Apple Silicon
(`macos-aarch64`) shipped. The build matrix in
`.github/workflows/release.yml` now includes a `macos-13` job for the
`x86_64-apple-darwin` target; from `v1.0.6` onward both Intel and
Apple Silicon assets ship side-by-side.

Older releases (`v1.0.5` and earlier): Intel Mac users must either run
the aarch64 binary under Rosetta 2 or build from source.
