#!/usr/bin/env bash
# verify-release-assets.sh — fail loudly when a GitHub Release is missing
# any of the binary assets the public npm packages (and the curl-based
# installer) depend on.
#
# Background: #418. SDK 1.0.5 shipped to npm with no
# `red-linux-x86_64` published on the matching GitHub Release, so every
# Linux x86_64 install 404'd until users discovered REDDB_BIN. The
# release workflow gates `publish-js-{driver,client,bun}` on this
# script — if any required asset is missing, npm publish never runs and
# the operator gets a clear list of what to upload (or which build
# matrix entry to re-run).
#
# Required assets are the cross-product of:
#   - bins:      red, red_client
#   - suffixes:  linux-x86_64, linux-aarch64, linux-armv7,
#                linux-x86_64-static, linux-aarch64-static,
#                windows-x86_64.exe
# The suffix list mirrors the required entries of `PLATFORMS` in
# `scripts/npm-platform-packages.mjs`, which builds the `@reddb-io/red-*` npm
# packages from these assets. macOS assets are temporarily optional because
# the hosted macOS runners have been the least stable part of the release
# path. The `-static` (musl) assets back the `-musl` npm packages.
#
# Usage:
#   GH_TOKEN=...  scripts/verify-release-assets.sh v1.0.8
#
# Exits 0 when every required asset is present, 1 otherwise. Prints the
# missing assets to stderr.

set -euo pipefail

TAG="${1:-}"
if [[ -z "$TAG" ]]; then
  echo "usage: $0 <release-tag>" >&2
  exit 2
fi

REPO="${GITHUB_REPOSITORY:-reddb-io/reddb}"

BINS=(red red_client)
SUFFIXES=(
  linux-x86_64
  linux-aarch64
  linux-armv7
  linux-x86_64-static
  linux-aarch64-static
  windows-x86_64.exe
)
EXTRA_ASSETS=(
  checksums.txt
  SHA256SUMS
  "red-${TAG}.spdx.json"
  "red-${TAG}.cyclonedx.json"
)

echo "verify-release-assets: checking ${REPO}@${TAG}"
ASSETS_JSON="$(gh release view "$TAG" --repo "$REPO" --json assets --jq '[.assets[].name]')"
echo "verify-release-assets: release lists $(jq 'length' <<<"$ASSETS_JSON") assets"

MISSING=()
for bin in "${BINS[@]}"; do
  for suffix in "${SUFFIXES[@]}"; do
    name="${bin}-${suffix}"
    if ! jq -e --arg n "$name" 'index($n)' <<<"$ASSETS_JSON" >/dev/null; then
      MISSING+=("$name")
    fi
  done
done
for name in "${EXTRA_ASSETS[@]}"; do
  if ! jq -e --arg n "$name" 'index($n)' <<<"$ASSETS_JSON" >/dev/null; then
    MISSING+=("$name")
  fi
done

if (( ${#MISSING[@]} > 0 )); then
  {
    echo
    echo "ERROR: release ${TAG} is missing ${#MISSING[@]} required asset(s):"
    for m in "${MISSING[@]}"; do echo "  - $m"; done
    echo
    echo "These assets back the npm platform packages, checksum verification, artifact attestations, and curl-based installer."
    echo "Do NOT publish to npm without them — see docs/release-runbook.md"
    echo "(\"Release asset contract\")."
  } >&2
  exit 1
fi

echo "verify-release-assets: all $((${#BINS[@]} * ${#SUFFIXES[@]})) required assets present."
