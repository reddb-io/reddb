#!/usr/bin/env bash
#
# release.sh — cut a release locally, end to end, with guard rails.
#
# Usage:
#   ./scripts/release.sh [patch|minor|major] ["summary line"]
#   ./scripts/release.sh                      # consume pending changesets as-is
#
# What it guarantees, in order, and why each step exists:
#
#   1. PREFLIGHT — refuses to run on a dirty tree, off `main`, or on a
#      `main` that is behind/ahead of `origin/main`. The stale-`main`
#      trap (local at an old version while the remote moved on) is what
#      produced the 1.2.x-vs-1.3.x version confusion; we catch it here
#      instead of bumping from the wrong base.
#   2. BUMP — drives the canonical Changesets path
#      (`changeset version` + `scripts/sync-version.js`) so EVERY
#      lock-stepped manifest moves together (npm packages, all crates,
#      Cargo.lock, python, bun, internal packages). The old version of
#      this script only bumped the Rust crates and silently broke
#      lock-step — that bug is gone.
#   3. GATE — runs `scripts/check-versions.sh` and ABORTS before the tag
#      if any file disagrees. The git tag is never created from an
#      inconsistent tree.
#   4. TAG — commits "version packages" and creates an annotated
#      `v<version>` tag. Does NOT push: pushing fires the publish
#      pipeline, so that stays a deliberate `make release-push`.
#
# After this, run `make release-push` then
# `./scripts/verify-release.sh v<version>` to confirm the tag, GitHub
# Release, and downloadable assets are all in sync.
set -euo pipefail
cd "$(dirname "$0")/.."

TYPE="${1:-}"
SUMMARY="${2:-}"

if [[ -n "$TYPE" && "$TYPE" != "patch" && "$TYPE" != "minor" && "$TYPE" != "major" ]]; then
  echo "❌ usage: $0 [patch|minor|major] [\"summary line\"]" >&2
  exit 1
fi

# ── 1. preflight ────────────────────────────────────────────────────
if [[ -n "$(git status --porcelain)" ]]; then
  echo "❌ working tree is not clean — commit or stash first." >&2
  exit 1
fi

BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$BRANCH" != "main" ]]; then
  echo "❌ releases are cut from 'main', you are on '$BRANCH'." >&2
  exit 1
fi

if ! command -v gh >/dev/null 2>&1 || ! gh auth status >/dev/null 2>&1; then
  echo "❌ 'gh' must be installed and authenticated (needed for the" >&2
  echo "   changelog lookup and post-release asset verification)." >&2
  exit 1
fi

echo "→ fetching origin/main to check for drift…"
git fetch --quiet origin main
LOCAL="$(git rev-parse @)"
REMOTE="$(git rev-parse origin/main)"
BASE="$(git merge-base @ origin/main)"
if [[ "$LOCAL" != "$REMOTE" ]]; then
  if [[ "$LOCAL" == "$BASE" ]]; then
    echo "❌ local 'main' is BEHIND origin/main — run 'git pull --ff-only' first." >&2
    echo "   (cutting from a stale main is what caused past version drift.)" >&2
  elif [[ "$REMOTE" == "$BASE" ]]; then
    echo "❌ local 'main' is AHEAD of origin/main — push your commits first." >&2
  else
    echo "❌ local 'main' has DIVERGED from origin/main — reconcile first." >&2
  fi
  exit 1
fi
echo "  ✓ main is in sync with origin/main"

# ── 2. bump every manifest in lock-step ─────────────────────────────
PENDING="$(find .changeset -maxdepth 1 -name '*.md' ! -name 'README.md' 2>/dev/null || true)"
if [[ -z "$PENDING" ]]; then
  if [[ -z "$TYPE" ]]; then
    echo "❌ no pending changesets and no bump type given." >&2
    echo "   run: $0 [patch|minor|major] [\"summary\"]" >&2
    exit 1
  fi
  SLUG=".changeset/release-$(date +%Y%m%d%H%M%S).md"
  [[ -z "$SUMMARY" ]] && SUMMARY="${TYPE} release"
  {
    echo "---"
    echo "\"@reddb-io/cli\": ${TYPE}"
    echo "---"
    echo
    echo "${SUMMARY}"
  } > "$SLUG"
  echo "  ✓ created changeset $SLUG (${TYPE})"
else
  echo "  ✓ using $(echo "$PENDING" | wc -l | tr -d ' ') pending changeset(s)"
fi

# `changeset version` renders the changelog via @changesets/changelog-github,
# which calls the GitHub API to resolve the PR/author for each changeset.
# That lookup needs a token AND a PR association; run locally (no PR yet) it
# throws "Cannot read properties of null (reading 'author')". We provide the
# token from gh, and if the GitHub renderer still fails we transparently fall
# back to the basic changelog so a local cut is never blocked by changelog
# decoration. CI keeps using the rich GitHub changelog.
export GITHUB_TOKEN="${GITHUB_TOKEN:-$(gh auth token 2>/dev/null || true)}"
echo "→ bumping versions (changeset version)…"
if ! pnpm exec changeset version >/tmp/reddb-changeset.log 2>&1; then
  echo "  · github changelog renderer failed locally — falling back to basic changelog"
  cp .changeset/config.json .changeset/config.json.relbak
  trap 'mv -f .changeset/config.json.relbak .changeset/config.json 2>/dev/null || true' EXIT
  node -e '
    const f=".changeset/config.json"; const c=require("./"+f);
    c.changelog="@changesets/cli/changelog";
    require("fs").writeFileSync(f, JSON.stringify(c,null,2)+"\n");
  '
  pnpm exec changeset version
  mv -f .changeset/config.json.relbak .changeset/config.json
  trap - EXIT
fi

echo "→ propagating version across lock-stepped manifests (sync-version.js)…"
node scripts/sync-version.js

# Refresh the workspace lockfile so Cargo.lock matches the bump. Best
# effort locally; release.yml does the authoritative --locked build.
cargo check >/dev/null 2>&1 || true

VERSION="$(node -e 'process.stdout.write(require("./package.json").version)')"
TAG="v$VERSION"

# ── 3. gate: every file must agree BEFORE we tag ────────────────────
echo "→ verifying lock-step before tagging…"
if ! bash scripts/check-versions.sh; then
  echo "❌ version drift detected — NOT tagging. Fix the files above and re-run." >&2
  exit 1
fi

# ── 4. commit + tag (no push) ───────────────────────────────────────
git add -A
git commit -q -m "chore(release): version packages"

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  echo "  · tag $TAG already exists locally — leaving it untouched"
else
  git tag -a "$TAG" -m "Release $TAG"
fi

echo
echo "✅ release $TAG prepared and verified."
echo "   commit: $(git rev-parse --short HEAD)  tag: $TAG"
echo
echo "next:"
echo "   make release-push                      # push main + tag → fires release.yml"
echo "   ./scripts/verify-release.sh $TAG       # confirm tag ↔ release ↔ assets in sync"
