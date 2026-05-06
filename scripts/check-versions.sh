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
