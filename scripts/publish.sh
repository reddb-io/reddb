#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-publish}"
if [ "$MODE" != "publish" ] && [ "$MODE" != "--dry-run" ] && [ "$MODE" != "--help" ] && [ "$MODE" != "-h" ]; then
  echo "❌ Invalid argument: $MODE"
  echo "Usage: ./scripts/publish.sh [--dry-run]"
  exit 1
fi

if [ "$MODE" = "--help" ] || [ "$MODE" = "-h" ]; then
  echo "Usage: ./scripts/publish.sh [--dry-run]"
  echo "  --dry-run  Run cargo publish --dry-run instead of publishing"
  exit 0
fi

if [ "$MODE" = "--dry-run" ]; then
  CMD=(cargo publish --locked --dry-run)
  echo "Running cargo publish --locked --dry-run"
else
  CMD=(cargo publish --locked)
  if [ -z "${CARGO_REGISTRY_TOKEN:-}" ]; then
    echo "Warning: CARGO_REGISTRY_TOKEN not set. Continuing with interactive login/token lookup."
  fi
fi

if [ -n "${CARGO_REGISTRY_TOKEN:-}" ]; then
  CMD+=(--token "$CARGO_REGISTRY_TOKEN")
fi

"${CMD[@]}"
