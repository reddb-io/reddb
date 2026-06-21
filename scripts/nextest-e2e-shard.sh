#!/usr/bin/env bash
# Run one shard of the RedDB e2e lane under cargo-nextest (issue #973).
#
# The e2e lane is the set of top-level integration-test binaries
# (filterset `kind(test)`). nextest's `--partition count:<i>/<N>` deterministically
# splits the selected tests into N buckets so each CI runner executes a disjoint
# slice; the union of all shards is the whole lane.
#
# Usage:
#   scripts/nextest-e2e-shard.sh <index> <total> [extra nextest args...]
#
# Example (four runners):
#   scripts/nextest-e2e-shard.sh 1 4
#   scripts/nextest-e2e-shard.sh 2 4
#   scripts/nextest-e2e-shard.sh 3 4
#   scripts/nextest-e2e-shard.sh 4 4
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <shard-index> <shard-total> [extra nextest args...]" >&2
  exit 2
fi

INDEX="$1"
TOTAL="$2"
shift 2

case "$INDEX" in (*[!0-9]*|'') echo "shard index must be a positive integer" >&2; exit 2;; esac
case "$TOTAL" in (*[!0-9]*|'') echo "shard total must be a positive integer" >&2; exit 2;; esac
if [ "$INDEX" -lt 1 ] || [ "$TOTAL" -lt 1 ] || [ "$INDEX" -gt "$TOTAL" ]; then
  echo "shard index must be in 1..=total ($INDEX/$TOTAL is invalid)" >&2
  exit 2
fi

PROFILE="${NEXTEST_PROFILE:-ci}"

exec cargo nextest run \
  --profile "$PROFILE" \
  --workspace \
  --locked \
  -E 'kind(test)' \
  --partition "count:${INDEX}/${TOTAL}" \
  "$@"
