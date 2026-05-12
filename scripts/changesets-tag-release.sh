#!/usr/bin/env bash
#
# changesets-tag-release.sh — wired in as the `publish` step of
# `changesets/action@v1` (see `.github/workflows/changesets.yml`).
#
# The action calls this only AFTER all pending changesets have been
# consumed (i.e. when the "Version Packages" PR was merged and the
# version-bump commit just landed on `main`). We:
#
#   1. Read the canonical version from the root package.json. The
#      `pnpm release:version` step that ran before us has already
#      written this value to every lock-stepped manifest via
#      `scripts/sync-version.js`.
#   2. Validate it looks like a release version (semver, no prerelease
#      suffix unless the operator opted into one).
#   3. Tag and push `v<version>`. The pushed tag triggers the existing
#      release.yml workflow, which builds the binaries and publishes
#      to npm / crates.io / PyPI / GHCR.
#
# Idempotent: if the tag already exists locally or remotely we exit 0
# without re-pushing — re-running this script must never break.
#
# Env:
#   GITHUB_TOKEN  Required, set by changesets/action via the workflow.
#                 Used implicitly by `git push` via the checkout token.
set -euo pipefail

VERSION="$(node -e 'process.stdout.write(require("./package.json").version)')"

if [[ -z "$VERSION" ]]; then
  echo "changesets-tag-release: package.json has no version" >&2
  exit 1
fi
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._-]+)?$ ]]; then
  echo "changesets-tag-release: version '$VERSION' is not a valid semver" >&2
  exit 1
fi

TAG="v$VERSION"

# If the tag already exists locally and matches HEAD, nothing to do.
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  echo "changesets-tag-release: tag $TAG already exists locally — skipping create"
else
  git tag -a "$TAG" -m "Release $TAG"
fi

# If the remote already has the tag, skip the push.
if git ls-remote --tags origin "refs/tags/$TAG" | grep -q "$TAG"; then
  echo "changesets-tag-release: tag $TAG already on origin — skipping push"
  exit 0
fi

git push origin "refs/tags/$TAG"
echo "changesets-tag-release: pushed $TAG — release.yml will take over"
