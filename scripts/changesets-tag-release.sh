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
#   3. Tag and push `v<version>`, then dispatch release.yml on that tag.
#      Checkout uses the workflow token, whose pushes do not trigger workflows.
#
# An existing tag without a publisher is recoverable. Never move the tag or
# submit another publisher for a commit that already has a release run.
#
# Env:
#   GH_TOKEN  Workflow token with Actions write permission, used by gh.
#             Git uses checkout's workflow-token credentials.
#   GITHUB_REPOSITORY  Repository receiving the release workflow dispatch.
set -euo pipefail

RELEASE_REPOSITORY="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must identify the release repository}"

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

RELEASE_REMOTE_REFS="$(git ls-remote --tags origin "refs/tags/$TAG" "refs/tags/$TAG^{}")"
if [[ -n "$RELEASE_REMOTE_REFS" ]]; then
  echo "changesets-tag-release: tag $TAG already on origin — skipping push"
else
  if ! git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    git tag -a "$TAG" -m "Release $TAG"
  fi
  git push origin "refs/tags/$TAG"
  RELEASE_REMOTE_REFS="$(git ls-remote --tags origin "refs/tags/$TAG" "refs/tags/$TAG^{}")"
fi

# Resolve the immutable remote tag, including recovery after main advanced.
RELEASE_COMMIT="$(printf '%s\n' "$RELEASE_REMOTE_REFS" | awk -v tag="$TAG" '
  $2 == "refs/tags/" tag { direct = $1 }
  $2 == "refs/tags/" tag "^{}" { peeled = $1 }
  END { print peeled != "" ? peeled : direct }
')"
if [[ ! "$RELEASE_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "changesets-tag-release: cannot resolve remote commit for $TAG" >&2
  exit 1
fi
RELEASE_EXISTING_RUNS="$(gh run list --repo "$RELEASE_REPOSITORY" --workflow release.yml \
  --branch "$TAG" --limit 100 --json databaseId,headSha \
  --jq ".[] | select(.headSha == \"$RELEASE_COMMIT\") | .databaseId")"
if [[ -n "$RELEASE_EXISTING_RUNS" ]]; then
  echo "changesets-tag-release: publisher already exists for $TAG ($RELEASE_COMMIT)"
  exit 0
fi
gh workflow run release.yml --repo "$RELEASE_REPOSITORY" --ref "$TAG" \
  -f channel=stable -f version="$VERSION" -f publish_cargo=true
echo "changesets-tag-release: dispatched publisher for $TAG ($RELEASE_COMMIT)"
