#!/usr/bin/env bash
# Enforce the repository-global stable version guard. The sole exception is a
# fail-closed maintenance release from release/1.36 at its exact remote HEAD.

set -euo pipefail

engine=${1:?usage: check-version-drift.sh <workspace-version>}
maintenance_branch=${REDDB_MAINTENANCE_RELEASE_BRANCH:-}

if [[ ! "$engine" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "  ✗ workspace version '$engine' is not a stable semantic version" >&2
  exit 1
fi

latest_tag=$(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname 2>/dev/null \
  | { grep -vE -- '-' || true; } | head -1 | sed 's/^v//')

maintenance_authorized=false
if [[ -n "$maintenance_branch" ]]; then
  expected_branch="release/${engine%.*}"
  expected_ref="refs/tags/v${engine}"

  if [[ "$maintenance_branch" != "release/1.36" ]]; then
    echo "  ✗ maintenance releases are authorized only from release/1.36" >&2
    exit 1
  fi
  if [[ "$maintenance_branch" != "$expected_branch" ]]; then
    echo "  ✗ maintenance branch $maintenance_branch must match workspace line $expected_branch" >&2
    exit 1
  fi
  if [[ "${GITHUB_REF:-}" != "$expected_ref" ]]; then
    echo "  ✗ maintenance release requires exact tag ref $expected_ref (got ${GITHUB_REF:-<unset>})" >&2
    exit 1
  fi

  head_commit=$(git rev-parse --verify 'HEAD^{commit}')
  tag_commit=$(git rev-parse --verify "${expected_ref}^{commit}" 2>/dev/null || true)
  if [[ -z "$tag_commit" || "$tag_commit" != "$head_commit" ]]; then
    echo "  ✗ exact maintenance tag v$engine must resolve to HEAD $head_commit" >&2
    exit 1
  fi

  remote_line=$(git ls-remote --exit-code origin "refs/heads/${maintenance_branch}" 2>/dev/null || true)
  remote_commit=${remote_line%%[[:space:]]*}
  if [[ -z "$remote_commit" || "$remote_commit" != "$head_commit" ]]; then
    echo "  ✗ HEAD $head_commit must equal remote $maintenance_branch HEAD${remote_commit:+ $remote_commit}" >&2
    exit 1
  fi

  maintenance_authorized=true
  echo "  ✓ authorized maintenance release v$engine at remote $maintenance_branch HEAD $head_commit"
fi

if [[ -z "$latest_tag" ]]; then
  echo "  · no stable vX.Y.Z tags visible (shallow clone?) — skipping drift guard"
elif [[ "$(printf '%s\n%s\n' "$latest_tag" "$engine" | sort -V | tail -1)" != "$engine" ]]; then
  if [[ "$maintenance_authorized" == true ]]; then
    echo "  ✓ maintenance $engine is behind global v$latest_tag but passed exact release-line authority"
  else
    echo "  ✗ committed version $engine is BEHIND latest published tag v$latest_tag" >&2
    echo "    a release bump did not land on main — cut releases via the Changesets" >&2
    echo "    'Version Packages' PR, not a manual release.yml dispatch." >&2
    echo "    See docs/release-runbook.md § Version integrity." >&2
    exit 1
  fi
else
  echo "  ✓ committed $engine is at or ahead of latest published tag v$latest_tag"
fi
