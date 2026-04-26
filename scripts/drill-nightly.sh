#!/usr/bin/env bash
# PLAN.md B3 — nightly backup/restore drill runner.

set -euo pipefail

cd "$(dirname "$0")/.."

OUT_DIR=docs/release
HISTORY="$OUT_DIR/drill-history.md"
mkdir -p "$OUT_DIR"

if [[ ! -f "$HISTORY" ]]; then
  {
    echo "# Backup/Restore Drill History"
    echo
    echo "| timestamp UTC | command | result |"
    echo "|---------------|---------|--------|"
  } > "$HISTORY"
fi

CMD="cargo test --locked --test 'drill_*' --no-fail-fast"
START="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
LOG="$(mktemp -t reddb-drill-nightly.XXXXXX.log)"

set +e
bash -lc "$CMD" >"$LOG" 2>&1
STATUS=$?
set -e

if [[ $STATUS -eq 0 ]]; then
  RESULT="PASS"
else
  RESULT="FAIL(exit=${STATUS})"
fi

printf '| %s | `%s` | %s |\n' "$START" "$CMD" "$RESULT" >> "$HISTORY"
cat "$LOG"
rm -f "$LOG"
exit "$STATUS"
