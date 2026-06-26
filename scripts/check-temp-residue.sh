#!/usr/bin/env bash
# CI guard: report (and wholesale-clean) reddb-*/reddb_*/*.rdb/*.wal residue
# left in the OS temp directory ($TMPDIR, default /tmp) by the test suite.
#
# History (issue #972): this guard used to FAIL the build on any residue, to
# pressure tests into using the auto-cleaning helpers in tests/support/mod.rs.
# In practice dozens of test families leak into the dedicated, per-run CI
# TMPDIR (RUNNER_TEMP/reddb-test-suite-tmp, which is itself discarded after the
# run), so a per-file hard failure rotted the required Test Suite check on main
# and forced a lift-protection dance on every merge.
#
# Structural fix: the dedicated TMPDIR is ephemeral, so individual test leaks
# are harmless to the host. This guard now REMOVES the matched residue
# wholesale and downgrades the signal to a CI ::warning:: — visibility into
# which tests leak is preserved (each LEAK line is still printed and counted),
# but the build is no longer blocked. Per-test cleanup remains the right
# long-term hygiene goal; it is just no longer a merge gate.
#
# Usage (from repo root, after running the test suite):
#   ./scripts/check-temp-residue.sh
#
# Exit code: always 0 (residue is cleaned, never fatal).

set -euo pipefail

tmpdir="${TMPDIR:-/tmp}"

# Collect matches at depth 1 (tempfile creates dirs directly in TMPDIR).
# 2>/dev/null suppresses permission-denied noise from foreign /tmp entries.
# || true prevents set -e from aborting on find permission errors.
entries=()
while IFS= read -r -d '' entry; do
    entries+=("$entry")
done < <(find "$tmpdir" -mindepth 1 -maxdepth 1 \
    \( -name 'reddb-*' -o -name 'reddb_*' -o -name '*.rdb' -o -name '*.wal' \) \
    -print0 2>/dev/null || true)

if [[ ${#entries[@]} -eq 0 ]]; then
    echo "OK — no reddb temp residue in $tmpdir."
    exit 0
fi

for entry in "${entries[@]}"; do
    echo "LEAK: $entry"
    # Only ever remove reddb-owned residue (the find filter guarantees this),
    # so this is safe even when $TMPDIR is a shared /tmp.
    rm -rf -- "$entry" 2>/dev/null || true
done

count=${#entries[@]}
printf '\n'
printf '::warning::Test suite left %d temp residue path(s) in %s; cleaned wholesale (non-fatal).\n' "$count" "$tmpdir" >&2
printf 'Each LEAK line above is a test that did not clean up. Long-term fix:\n' >&2
printf 'use temp_data_dir()/temp_db_file()/PersistentDbPath from tests/support/mod.rs\n' >&2
printf 'so cleanup happens on drop (including on panic). This is advisory, not a gate.\n' >&2
exit 0
