#!/usr/bin/env bash
# Workspace version consistency check.
# Fails if engine version drifts from any driver/SDK that should track it in lock-step.
#
# Run: bash scripts/check-versions.sh
# CI:  add to release-pipeline preflight.

set -euo pipefail
cd "$(dirname "$0")/.."

ENGINE=$(grep -m1 '^version' Cargo.toml | sed -E 's/version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')
echo "engine: $ENGINE"

fail=0

check() {
  local label=$1
  local actual=$2
  local expected=${3:-$ENGINE}
  if [[ "$actual" != "$expected" ]]; then
    echo "  ✗ $label is $actual (expected $expected)"
    fail=1
  else
    echo "  ✓ $label = $actual"
  fi
}

lock_version() {
  local lockfile=$1
  local package=$2
  awk -v package="$package" '
    $0 == "[[package]]" { in_package=0 }
    $0 == "name = \"" package "\"" { in_package=1 }
    in_package && /^version = / {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' "$lockfile"
}

# Lock-step with engine: publishable workspace member crates, drivers, npm package
check "crates/reddb-types"       "$(grep -m1 '^version' crates/reddb-types/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-crypto"      "$(grep -m1 '^version' crates/reddb-crypto/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-file"        "$(grep -m1 '^version' crates/reddb-file/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-rql"         "$(grep -m1 '^version' crates/reddb-rql/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-wire"        "$(grep -m1 '^version' crates/reddb-wire/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-grpc-proto"  "$(grep -m1 '^version' crates/reddb-grpc-proto/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-server"      "$(grep -m1 '^version' crates/reddb-server/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-client"            "$(grep -m1 '^version' crates/reddb-client/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-client-connector"  "$(grep -m1 '^version' crates/reddb-client-connector/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "Cargo.lock reddb-io"         "$(lock_version Cargo.lock reddb-io)"
check "Cargo.lock reddb-io-types"   "$(lock_version Cargo.lock reddb-io-types)"
check "Cargo.lock reddb-io-crypto"  "$(lock_version Cargo.lock reddb-io-crypto)"
check "Cargo.lock reddb-io-file"    "$(lock_version Cargo.lock reddb-io-file)"
check "Cargo.lock reddb-io-rql"     "$(lock_version Cargo.lock reddb-io-rql)"
check "Cargo.lock reddb-io-wire"    "$(lock_version Cargo.lock reddb-io-wire)"
check "Cargo.lock reddb-io-grpc-proto" "$(lock_version Cargo.lock reddb-io-grpc-proto)"
check "Cargo.lock reddb-io-server"  "$(lock_version Cargo.lock reddb-io-server)"
check "Cargo.lock reddb-io-client"  "$(lock_version Cargo.lock reddb-io-client)"
check "Cargo.lock reddb-io-client-connector" "$(lock_version Cargo.lock reddb-io-client-connector)"
check "drivers/python"      "$(grep -m1 '^version'  drivers/python/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "drivers/python (py)" "$(grep -m1 '^version' drivers/python/pyproject.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "drivers/python/Cargo.lock reddb-io" "$(lock_version drivers/python/Cargo.lock reddb-io)"
check "drivers/python/Cargo.lock reddb-io-python" "$(lock_version drivers/python/Cargo.lock reddb-io-python)"
check "drivers/js (@reddb-io/sdk)"  "$(grep -m1 '"version"' drivers/js/package.json | sed -E 's/.*"([0-9][^"]+)".*/\1/')"
check "drivers/bun (@reddb-io/client-bun)"  "$(grep -m1 '"version"' drivers/bun/package.json | sed -E 's/.*"([0-9][^"]+)".*/\1/')"
check "packages/internal-asset-fetcher" "$(grep -m1 '"version"' packages/internal-asset-fetcher/package.json | sed -E 's/.*"([0-9][^"]+)".*/\1/')"
check "packages/internal-bin-resolver"  "$(grep -m1 '"version"' packages/internal-bin-resolver/package.json | sed -E 's/.*"([0-9][^"]+)".*/\1/')"
check "packages/internal-version-compare" "$(grep -m1 '"version"' packages/internal-version-compare/package.json | sed -E 's/.*"([0-9][^"]+)".*/\1/')"
# drivers/js-client is being introduced by Lane T (#136) in parallel.
# Skip gracefully if the manifest isn't on this branch yet — the line
# becomes load-bearing once both lanes merge.
if [[ -f drivers/js-client/package.json ]]; then
  check "drivers/js-client (@reddb-io/client)" "$(grep -m1 '"version"' drivers/js-client/package.json | sed -E 's/.*"([0-9][^"]+)".*/\1/')"
else
  echo "  · drivers/js-client (@reddb-io/client) — not present yet, skipping (Lane T #136)"
fi
check "@reddb-io/cli"               "$(grep -m1 '"version"' package.json | sed -E 's/.*"([0-9][^"]+)".*/\1/')"

node scripts/check-registry-names.mjs
node scripts/check-contract-authorities.mjs

# Drift guard: the committed version must never be BEHIND the latest published
# stable tag. Catches the failure mode where a release was cut without the
# version bump landing back on main (e.g. a manual release.yml dispatch instead
# of merging the Changesets "Version Packages" PR), leaving every committed
# manifest stale — exactly the 1.2.0-vs-v1.2.5 drift this guard was added for.
echo
latest_tag=$(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname 2>/dev/null \
  | { grep -vE -- '-' || true; } | head -1 | sed 's/^v//')
if [[ -z "$latest_tag" ]]; then
  echo "  · no stable vX.Y.Z tags visible (shallow clone?) — skipping drift guard"
elif [[ "$(printf '%s\n%s\n' "$latest_tag" "$ENGINE" | sort -V | tail -1)" != "$ENGINE" ]]; then
  echo "  ✗ committed version $ENGINE is BEHIND latest published tag v$latest_tag"
  echo "    a release bump did not land on main — cut releases via the Changesets"
  echo "    'Version Packages' PR, not a manual release.yml dispatch."
  echo "    See docs/release-runbook.md § Version integrity."
  fail=1
else
  echo "  ✓ committed $ENGINE is at or ahead of latest published tag v$latest_tag"
fi

# Independent versions (informational only)
echo
echo "independent (no lock-step):"
echo "  · drivers/python-asyncio = $(grep -m1 '^version' drivers/python-asyncio/pyproject.toml | sed -E 's/.*"([^"]+)".*/\1/')"
echo "  · charts/reddb           = $(grep -m1 '^version:' charts/reddb/Chart.yaml | awk '{print $2}')"

if (( fail )); then
  echo
  echo "version drift detected — bump together or document the divergence."
  exit 1
fi
echo
echo "all lock-stepped versions match $ENGINE"
