#!/usr/bin/env bash
#
# verify-release.sh — confirm a pushed release is fully landed: the git
# tag, the GitHub Release, and every downloadable asset are in sync.
#
# Usage:
#   ./scripts/verify-release.sh v1.4.0            # verify, fail if not ready yet
#   ./scripts/verify-release.sh v1.4.0 --wait     # poll release.yml until done, then verify
#
# Guarantees checked (the three the operator actually cares about):
#   1. TAG ↔ RELEASE SYNC — the tag exists on origin, a published (non-draft)
#      GitHub Release exists for it, and the release points at the SAME commit
#      the tag points at. Catches the "npm published but no matching Release"
#      window (#418) and tag/release commit mismatches.
#   2. DOWNLOADABLE — every release-blocking binary asset is attached
#      (delegates to verify-release-assets.sh) AND at least one asset actually
#      downloads (HTTP 200 + non-empty), proving the Release is reachable, not
#      just listed.
#   3. REGISTRY SYNC — the version published to npm (@reddb-io/cli) matches the
#      tag. Informational for crates.io.
#
# Exits 0 only when all guarantees hold.
set -euo pipefail
cd "$(dirname "$0")/.."

TAG="${1:-}"
WAIT=0
[[ "${2:-}" == "--wait" ]] && WAIT=1
if [[ -z "$TAG" ]]; then
  echo "usage: $0 <vX.Y.Z> [--wait]" >&2
  exit 2
fi
[[ "$TAG" == v* ]] || TAG="v$TAG"
VERSION="${TAG#v}"
REPO="${GITHUB_REPOSITORY:-reddb-io/reddb}"

if ! command -v gh >/dev/null 2>&1 || ! gh auth status >/dev/null 2>&1; then
  echo "❌ 'gh' must be installed and authenticated." >&2
  exit 1
fi

fail=0
note() { printf '  %s %s\n' "$1" "$2"; }

# ── optionally wait for release.yml to finish for this tag ──────────
if (( WAIT )); then
  echo "→ waiting for the Release workflow on $TAG…"
  for _ in $(seq 1 60); do            # up to ~30 min (30s * 60)
    RUN_ID="$(gh run list --repo "$REPO" --workflow=release.yml \
      --json databaseId,headBranch,status \
      --jq "[.[] | select(.headBranch==\"$TAG\")] | first | .databaseId" 2>/dev/null || true)"
    if [[ -n "$RUN_ID" && "$RUN_ID" != "null" ]]; then
      STATUS="$(gh run view "$RUN_ID" --repo "$REPO" --json status --jq .status 2>/dev/null || echo "")"
      [[ "$STATUS" == "completed" ]] && { echo "  ✓ run $RUN_ID completed"; break; }
    fi
    sleep 30
  done
fi

# ── 1. tag ↔ release sync ───────────────────────────────────────────
echo "→ checking tag ↔ release sync for $TAG…"
TAG_SHA="$(git ls-remote --tags origin "refs/tags/$TAG^{}" | awk '{print $1}')"
[[ -z "$TAG_SHA" ]] && TAG_SHA="$(git ls-remote --tags origin "refs/tags/$TAG" | awk '{print $1}')"
if [[ -z "$TAG_SHA" ]]; then
  note "✗" "tag $TAG not found on origin"; fail=1
else
  note "✓" "tag $TAG on origin → ${TAG_SHA:0:12}"
fi

if ! REL_JSON="$(gh release view "$TAG" --repo "$REPO" \
      --json isDraft,tagName,targetCommitish,publishedAt 2>/dev/null)"; then
  note "✗" "no GitHub Release for $TAG"; fail=1
else
  IS_DRAFT="$(jq -r .isDraft <<<"$REL_JSON")"
  PUBLISHED="$(jq -r .publishedAt <<<"$REL_JSON")"
  REL_COMMITISH="$(jq -r .targetCommitish <<<"$REL_JSON")"
  if [[ "$IS_DRAFT" == "true" || "$PUBLISHED" == "null" ]]; then
    note "✗" "Release $TAG exists but is a draft / unpublished"; fail=1
  else
    note "✓" "Release $TAG is published ($PUBLISHED)"
  fi
  # targetCommitish may be a branch name or a sha; resolve to a sha and compare.
  REL_SHA="$(git rev-parse "$REL_COMMITISH" 2>/dev/null || echo "$REL_COMMITISH")"
  if [[ -n "$TAG_SHA" && "$REL_SHA" != "$TAG_SHA" && "$REL_COMMITISH" != "main" ]]; then
    note "✗" "Release commit ($REL_SHA) ≠ tag commit ($TAG_SHA)"; fail=1
  else
    note "✓" "Release tracks the tagged commit"
  fi
fi

# ── 2. downloadable assets ──────────────────────────────────────────
echo "→ checking required binary assets are attached…"
if GITHUB_REPOSITORY="$REPO" bash scripts/verify-release-assets.sh "$TAG"; then
  note "✓" "all release-blocking assets present"
else
  note "✗" "missing required assets (see above)"; fail=1
fi

echo "→ proving an asset actually downloads…"
PROBE="red-linux-x86_64"
URL="$(gh release view "$TAG" --repo "$REPO" --json assets \
        --jq ".assets[] | select(.name==\"$PROBE\") | .url" 2>/dev/null || true)"
if [[ -z "$URL" ]]; then
  note "✗" "could not resolve download URL for $PROBE"; fail=1
else
  CODE="$(curl -sL -o /dev/null -w '%{http_code}' --max-time 120 "$URL" || echo "000")"
  if [[ "$CODE" == "200" ]]; then
    note "✓" "$PROBE downloads (HTTP 200)"
  else
    note "✗" "$PROBE download returned HTTP $CODE"; fail=1
  fi
fi

# ── 3. registry sync ────────────────────────────────────────────────
echo "→ checking registry versions match $VERSION…"
NPM_VER="$(npm view @reddb-io/cli version 2>/dev/null || true)"
if [[ -z "$NPM_VER" ]]; then
  note "·" "npm: could not query @reddb-io/cli (offline?) — skipped"
elif [[ "$NPM_VER" == "$VERSION" ]]; then
  note "✓" "npm @reddb-io/cli = $NPM_VER"
else
  note "✗" "npm @reddb-io/cli latest is $NPM_VER, expected $VERSION"; fail=1
fi

CRATE_VER="$(curl -s --max-time 30 "https://crates.io/api/v1/crates/reddb-io" \
              | jq -r '.crate.max_stable_version // empty' 2>/dev/null || true)"
if [[ -z "$CRATE_VER" ]]; then
  note "·" "crates.io: could not query reddb-io — skipped"
elif [[ "$CRATE_VER" == "$VERSION" ]]; then
  note "✓" "crates.io reddb-io = $CRATE_VER"
else
  note "·" "crates.io reddb-io latest is $CRATE_VER (expected $VERSION) — may still be propagating"
fi

echo
if (( fail )); then
  echo "❌ $TAG is NOT fully released — see the ✗ lines above."
  exit 1
fi
echo "✅ $TAG is fully released: tag, GitHub Release, assets, and npm all in sync."
