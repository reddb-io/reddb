#!/usr/bin/env bash
# PLAN.md B1 — cold-start P95 driver.
#
# Runs the cold_start_bench example across:
#   - sizes:     100 MB / 1 GB / 5 GB  (override via COLD_START_SIZES)
#   - scenarios: warm / cold_remote    (override via COLD_START_SCENARIOS)
#
# Aggregates JSON output into bench/cold-start-baseline.md.
#
# Targets per PLAN.md B1:
#   <2s P95 with volume present (warm)
#   <10s P95 from empty volume + remote (1 GB DB, cold_remote)
#
# Examples:
#   ./scripts/cold-start-bench.sh
#   COLD_START_SIZES="100 1024" ./scripts/cold-start-bench.sh
#   COLD_START_SCENARIOS="warm" COLD_START_ITERS=50 ./scripts/cold-start-bench.sh

set -euo pipefail

cd "$(dirname "$0")/.."

SIZES="${COLD_START_SIZES:-100 1024 5120}"
SCENARIOS="${COLD_START_SCENARIOS:-warm cold_remote}"
ITERS="${COLD_START_ITERS:-20}"
WARMUP="${COLD_START_WARMUP:-2}"

OUT_DIR=bench
mkdir -p "$OUT_DIR"
RAW_LOG="$OUT_DIR/cold-start-raw.jsonl"
SUMMARY="$OUT_DIR/cold-start-baseline.md"
: > "$RAW_LOG"

# Honour CARGO_TARGET_DIR / [build] target-dir / ~/.cargo/config.toml.
TARGET_DIR=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')
BIN="$TARGET_DIR/release/examples/cold_start_bench"

if [[ "${COLD_START_SKIP_BUILD:-}" != "1" ]]; then
  echo "[cold-start-bench] building example in release"
  cargo build --release --example cold_start_bench
fi
[[ -x "$BIN" ]] || { echo "missing $BIN — set COLD_START_SKIP_BUILD=0 to rebuild"; exit 1; }

run_one() {
  local scenario=$1
  local size_mb=$2
  echo "[cold-start-bench] running scenario=${scenario} size=${size_mb}MB iters=${ITERS} warmup=${WARMUP}"
  COLD_START_SCENARIO="$scenario" COLD_START_SIZE_MB="$size_mb" \
    COLD_START_ITERS="$ITERS" COLD_START_WARMUP="$WARMUP" \
    "$BIN" | tee -a "$RAW_LOG"
}

{
  echo "# Cold-Start Baseline"
  echo
  echo "Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo
  echo "Host: \`$(uname -srm)\`"
  echo "Toolchain: \`$(rustc --version)\`"
  echo
  echo "Source: \`examples/cold_start_bench.rs\` driven by \`scripts/cold-start-bench.sh\`."
  echo "Iterations per cell: ${ITERS} (+ ${WARMUP} warmup discarded)."
  echo
  echo "## PLAN.md B1 targets"
  echo
  echo "- \`warm\`: open P95 < **2000 ms** (data dir present, fresh process)."
  echo "- \`cold_remote\`: open P95 < **10000 ms** (empty data dir, restore-from-remote, 1 GB DB)."
  echo
} > "$SUMMARY"

emit_table() {
  local scenario=$1
  local title=$2
  echo "## Scenario: \`${scenario}\` — ${title}"
  echo
  echo "| size MB | open p50 ms | open p95 ms | open p99 ms | total p50 ms | total p95 ms | total p99 ms | restore p50 ms | restore p95 ms |"
  echo "|--------:|-----------:|-----------:|-----------:|------------:|------------:|------------:|--------------:|--------------:|"
}

for SCEN in $SCENARIOS; do
  case "$SCEN" in
    warm)        TITLE="data dir present, fresh process";;
    volume_only) TITLE="alias of warm under fresh-process bench";;
    cold_remote) TITLE="empty data dir, auto-restore from LocalBackend";;
    *) TITLE="";;
  esac
  emit_table "$SCEN" "$TITLE" >> "$SUMMARY"
  for SIZE in $SIZES; do
    JSON=$(run_one "$SCEN" "$SIZE" | tail -n1)
    python3 - "$JSON" >> "$SUMMARY" <<'PY'
import json, sys
r = json.loads(sys.argv[1])
print(f"| {r['size_mb']} | {r['open_ms_p50']} | {r['open_ms_p95']} | {r['open_ms_p99']} | {r['total_ms_p50']} | {r['total_ms_p95']} | {r['total_ms_p99']} | {r.get('restore_ms_p50', 0)} | {r.get('restore_ms_p95', 0)} |")
PY
  done
  echo "" >> "$SUMMARY"
done

{
  echo "## Gate verdict"
  echo
  echo "Compare \`open p95\` columns above against the targets in PLAN.md B1."
  echo "Numbers under the threshold close B1; numbers over the threshold escalate to Phase 9.2 (incremental snapshot) or the cold-start-budget milestone."
} >> "$SUMMARY"

echo "[cold-start-bench] wrote $SUMMARY"
