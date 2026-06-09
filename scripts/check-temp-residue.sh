#!/usr/bin/env bash
# CI guard: fail if the test suite left reddb-*/reddb_*/*.rdb/*.wal
# residue in the OS temp directory ($TMPDIR, default /tmp).
#
# Run this after `cargo test` to detect tests that created persistent
# databases without using the auto-cleaning temp helpers in
# tests/support/mod.rs, turning silent disk leaks into a hard CI failure.
#
# Usage (from repo root, after running the test suite):
#   ./scripts/check-temp-residue.sh
#
# Exit codes:
#   0 - no residue found
#   1 - residue found (paths listed to stdout)

set -euo pipefail

tmpdir="${TMPDIR:-/tmp}"

# Collect matches at depth 1 (tempfile creates dirs directly in TMPDIR).
# 2>/dev/null suppresses permission-denied noise from foreign /tmp entries.
# || true prevents set -e from aborting on find permission errors.
entries=()
while IFS= read -r -d '' entry; do
    entries+=("$entry")
done < <(find "$tmpdir" -maxdepth 1 \
    \( -name 'reddb-*' -o -name 'reddb_*' -o -name '*.rdb' -o -name '*.wal' \) \
    -print0 2>/dev/null || true)

if [[ ${#entries[@]} -eq 0 ]]; then
    echo "OK — no reddb temp residue in $tmpdir."
    exit 0
fi

for entry in "${entries[@]}"; do
    echo "LEAK: $entry"
done

count=${#entries[@]}
printf '\n'
printf '::error::Test suite left %d temp residue path(s) in %s.\n' "$count" "$tmpdir" >&2
printf 'Each LEAK line above is a file or directory that was not cleaned up.\n' >&2
printf 'Fix: use temp_data_dir()/temp_db_file()/PersistentDbPath from tests/support/mod.rs\n' >&2
printf 'so cleanup happens on drop (including on panic).\n' >&2
exit 1
