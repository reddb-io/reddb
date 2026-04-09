#!/usr/bin/env bash
set -e

# Usage: ./scripts/release.sh [patch|minor|major]
TYPE="$1"
if [ -z "$TYPE" ]; then
  TYPE="patch"
fi

if [ "$TYPE" != "patch" ] && [ "$TYPE" != "minor" ] && [ "$TYPE" != "major" ]; then
  echo "❌ Usage: ./scripts/release.sh [patch|minor|major]"
  exit 1
fi

if [ -n "$(git status --porcelain)" ]; then
  echo "❌ Error: Git working directory is not clean."
  echo "Commit or stash your changes before creating a release."
  exit 1
fi

CURRENT_VERSION=$(awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}' Cargo.toml)
if [ -z "$CURRENT_VERSION" ]; then
  echo "❌ Error: Failed to read current version from Cargo.toml."
  exit 1
fi

IFS='.' read -r -a PARTS <<< "$CURRENT_VERSION"
MAJOR="${PARTS[0]}"
MINOR="${PARTS[1]}"
PATCH="${PARTS[2]}"

case "$TYPE" in
  major)
    MAJOR=$((MAJOR + 1))
    MINOR=0
    PATCH=0
    ;;
  minor)
    MINOR=$((MINOR + 1))
    PATCH=0
    ;;
  patch)
    PATCH=$((PATCH + 1))
    ;;
esac

NEW_VERSION="${MAJOR}.${MINOR}.${PATCH}"
echo "Current: $CURRENT_VERSION"
echo "New:     $NEW_VERSION"

sed -i "s/^version = \"${CURRENT_VERSION}\"/version = \"${NEW_VERSION}\"/" Cargo.toml
cargo check > /dev/null 2>&1 || true

git add Cargo.toml Cargo.lock
git commit -m "chore: release v${NEW_VERSION}"
git tag "v${NEW_VERSION}"

echo "✅ Release prepared: v${NEW_VERSION}"
echo "👉 Run: git push --follow-tags"

