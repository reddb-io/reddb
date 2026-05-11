#!/usr/bin/env bash
# Workspace version consistency check.
# Fails if engine version drifts from any driver/SDK that should track it in lock-step.
#
# Run: bash scripts/check-versions.sh
# CI:  add to release-pipeline preflight.

set -euo pipefail
cd "$(dirname "$0")/.."

ENGINE=$(grep -m1 '^version' Cargo.toml | sed -E 's/version\s*=\s*"([^"]+)".*/\1/')
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

# Lock-step with engine: workspace member crates, drivers, npm package
check "crates/reddb-wire"        "$(grep -m1 '^version' crates/reddb-wire/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-grpc-proto"  "$(grep -m1 '^version' crates/reddb-grpc-proto/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-server"      "$(grep -m1 '^version' crates/reddb-server/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-client"            "$(grep -m1 '^version' crates/reddb-client/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "crates/reddb-client-connector"  "$(grep -m1 '^version' crates/reddb-client-connector/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "drivers/python"      "$(grep -m1 '^version'  drivers/python/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "drivers/python (py)" "$(grep -m1 '^version' drivers/python/pyproject.toml | sed -E 's/.*"([^"]+)".*/\1/')"
check "drivers/js (@reddb-io/sdk)"  "$(grep -m1 '"version"' drivers/js/package.json | sed -E 's/.*"([0-9][^"]+)".*/\1/')"
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
