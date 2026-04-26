# Releasing RedDB

End-to-end runbook for cutting a stable release of `reddb` (engine) +
`reddb-client` (Rust driver) to crates.io, GitHub Releases, npm, and
Docker Hub.

## Prerequisites (one-time setup)

GitHub Actions secrets the `release.yml` workflow consumes:

| Secret                 | Used by                          |
|------------------------|----------------------------------|
| `CARGO_REGISTRY_TOKEN` | `publish-cargo`, `publish-rust-client` |
| `NPM_TOKEN`            | `publish-npm`, `publish-js-driver` |
| `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN` | `publish-docker` |
| `PYPI_TOKEN` (optional) | `publish-python-upload` (currently disabled) |

Verify they exist before the first stable tag:

```bash
gh secret list -R forattini-dev/reddb
```

## Stable release

From a clean working tree on `main`:

```bash
make patch     # 0.2.4 → 0.2.5
# or
make minor     # 0.2.4 → 0.3.0
# or
make major     # 0.2.4 → 1.0.0
```

The `release.sh` helper:
1. Bumps the version in `Cargo.toml` and `drivers/rust/Cargo.toml`.
2. Refreshes both `Cargo.lock` files via `cargo check`.
3. Stages and commits as `chore: release vX.Y.Z`.
4. Tags `vX.Y.Z`.

Then push:

```bash
make release-push
# == git push --follow-tags
```

The tag push triggers `.github/workflows/release.yml`, which:

1. **plan** — decides channel (stable for tags, next for main pushes).
2. **build** — cross-compiles the `red` binary for 6 targets
   (linux x86_64/aarch64/armv7/aarch64-musl, macos arm64, windows x86_64).
3. **artifact-sizes** — runs `make artifact-size` and asserts the
   binary stays under 30 MB and the container under 50 MB
   (PLAN.md B2 gate).
4. **publish-github** — creates the GitHub Release with all built binaries.
5. **publish-cargo** — `cargo publish` the engine to crates.io.
6. **publish-rust-client** — waits for the engine to appear on
   the crates.io sparse index, then publishes `reddb-client`.
7. **publish-npm**, **publish-js-driver** — npm packages.
8. **publish-docker** — multi-arch Docker images.

Each step's status visible in `gh run watch`.

## Pre-flight checks

Before running `make patch|minor|major`:

```bash
make package-check     # cargo package dry-run for engine + driver
cargo test --lib       # unit tests
cargo test --test 'chaos_*'  # chaos suite
cargo test --test 'drill_*'  # backup/restore drills
```

The CI `publish-dry-run` job runs `cargo package` on every PR, so
packaging issues are caught before merge.

## Next-channel (prerelease) flow

Pushes to `main` without a tag automatically publish a `next` build
to GitHub Releases (binary artifacts only — `publish-cargo` /
`publish-rust-client` are gated on `release_channel == 'stable'` and
do not fire on next-channel runs).

Manual dispatch:

```bash
gh workflow run release.yml -f channel=next -f version=0.2.4-next.42
```

## Recovery

### Tag pushed but workflow failed midway

- crates.io publishes are immutable; if `publish-cargo` succeeded but
  `publish-rust-client` failed, fix the driver and bump only the
  driver's version (`drivers/rust/Cargo.toml`), then `cargo publish`
  manually from `drivers/rust/`.
- GitHub Release can be re-cut by deleting the release (not the tag)
  and re-running the workflow.

### Wrong version tagged

```bash
git tag -d vX.Y.Z
git push origin --delete vX.Y.Z
```

Then bump again. **Never re-tag the same version on crates.io** —
crates.io rejects re-uploads.

## What lives where

| File                                   | Purpose |
|----------------------------------------|---------|
| `scripts/release.sh`                   | Local version bump + tag |
| `scripts/publish.sh`                   | Manual `cargo publish` (engine only) |
| `Makefile` (`patch`/`minor`/`major`)   | Wraps `release.sh` |
| `Makefile` (`release-push`)            | `git push --follow-tags` |
| `Makefile` (`package-check`)           | Local `cargo package` dry-run |
| `.github/workflows/release.yml`        | The full release pipeline |
| `.github/workflows/ci.yml`             | PR-time `publish-dry-run` gate |
| `Cargo.toml` `[package].include`       | Whitelist of files in the published tarball |
| `RELEASING.md`                         | This file |
