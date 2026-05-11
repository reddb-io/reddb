#!/usr/bin/env bash
# CI guard: fail when the stripped release `red_client` binary
# exceeds the size budget tracked in
# `crates/reddb-client/SIZE_BUDGET` (bytes, single integer line).
#
# The budget exists to catch accidental re-linkage of the engine
# (storage / runtime / replication / MCP / AI / server modules)
# into the thin client. A clean red_client binary is ~1.8 MB
# stripped; the budget is set well above that to allow legitimate
# growth (HTTPS / RedWire / TLS connectors) but well below the
# tens of megabytes engine re-linkage would add.
#
# Usage (from repo root):
#   ./scripts/check-red-client-size.sh

set -euo pipefail

BUDGET_FILE="crates/reddb-client/SIZE_BUDGET"
BIN_NAME="red_client"

if [[ ! -f "$BUDGET_FILE" ]]; then
  echo "error: size budget file missing at $BUDGET_FILE" >&2
  exit 1
fi
budget=$(grep -m1 -E '^[0-9]+$' "$BUDGET_FILE" || true)
if [[ -z "$budget" ]]; then
  echo "error: $BUDGET_FILE must contain a single integer (bytes)" >&2
  exit 1
fi

cargo build --locked --release --bin "$BIN_NAME" -p reddb-io-client --no-default-features

# Resolve target dir: respect CARGO_TARGET_DIR if set, otherwise
# the workspace default.
target_dir="${CARGO_TARGET_DIR:-target}"
src="$target_dir/release/$BIN_NAME"
if [[ ! -f "$src" ]]; then
  echo "error: built binary not found at $src" >&2
  exit 1
fi

# Strip a copy so the original keeps its symbols for local debugging.
stripped=$(mktemp)
trap 'rm -f "$stripped"' EXIT
cp "$src" "$stripped"
strip -s "$stripped"

size=$(stat -c%s "$stripped" 2>/dev/null || stat -f%z "$stripped")
printf 'red_client stripped size: %s bytes\n' "$size"
printf 'red_client size budget:   %s bytes\n' "$budget"

if (( size > budget )); then
  echo "::error::red_client stripped size $size exceeds budget $budget" >&2
  echo "Most likely cause: an engine module (storage / runtime / replication / MCP / AI / server) was pulled into the client crate's dependency tree." >&2
  echo "If the growth is intentional, raise the budget in $BUDGET_FILE in the same PR." >&2
  exit 1
fi

echo "OK — red_client size within budget."
