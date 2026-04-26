#!/usr/bin/env bash
# PLAN.md B2 — release artifact size measurement.

set -euo pipefail

cd "$(dirname "$0")/.."

MODE="${1:-all}"
OUT_DIR=bench
SUMMARY="$OUT_DIR/artifact-sizes.md"
mkdir -p "$OUT_DIR"

timestamp() {
  date -u +%Y-%m-%dT%H:%M:%SZ
}

human_bytes() {
  python3 - "$1" <<'PY'
import sys
n = int(sys.argv[1])
for unit in ["B", "KB", "MB", "GB"]:
    if n < 1024 or unit == "GB":
        print(f"{n:.2f} {unit}" if unit != "B" else f"{n} B")
        break
    n /= 1024
PY
}

write_header() {
  {
    echo "# Artifact Sizes"
    echo
    echo "Generated: $(timestamp)"
    echo
    echo "Targets from PLAN.md B2:"
    echo
    echo "- Static Linux binary: < **30 MB**."
    echo "- Container image: < **50 MB**."
    echo
    echo "| artifact | bytes | human | target | verdict |"
    echo "|----------|------:|-------|--------|---------|"
  } > "$SUMMARY"
}

append_row() {
  local artifact=$1
  local bytes=$2
  local target=$3
  local verdict=$4
  echo "| ${artifact} | ${bytes} | $(human_bytes "$bytes") | ${target} | ${verdict} |" >> "$SUMMARY"
}

measure_binary() {
  echo "[artifact-size] building release-static red binary"
  cargo build --locked --profile release-static --bin red >/dev/null
  local bin="target/release-static/red"
  if [[ ! -f "$bin" ]]; then
    bin="target/release-static/red.exe"
  fi
  [[ -f "$bin" ]] || { echo "missing release-static binary"; exit 1; }
  local bytes
  bytes=$(wc -c < "$bin" | tr -d ' ')
  local limit=$((30 * 1024 * 1024))
  local verdict="PASS"
  if (( bytes >= limit )); then
    verdict="FAIL"
  fi
  append_row "release-static red" "$bytes" "< 30 MB" "$verdict"
  echo "[artifact-size] binary bytes=$bytes verdict=$verdict"
  [[ "$verdict" == "PASS" ]]
}

measure_image() {
  command -v docker >/dev/null 2>&1 || {
    echo "[artifact-size] docker not found; cannot measure image"
    exit 1
  }
  local image="${REDDB_SIZE_IMAGE:-reddb:size-check}"
  echo "[artifact-size] building Docker image ${image}"
  docker build -t "$image" . >/dev/null
  local bytes
  bytes=$(docker image inspect "$image" --format '{{.Size}}')
  local limit=$((50 * 1024 * 1024))
  local verdict="PASS"
  if (( bytes >= limit )); then
    verdict="FAIL"
  fi
  append_row "container image ${image}" "$bytes" "< 50 MB" "$verdict"
  echo "[artifact-size] image bytes=$bytes verdict=$verdict"
  [[ "$verdict" == "PASS" ]]
}

write_header
case "$MODE" in
  binary)
    measure_binary
    ;;
  image)
    measure_image
    ;;
  all)
    measure_binary
    measure_image
    ;;
  *)
    echo "usage: $0 [binary|image|all]" >&2
    exit 2
    ;;
esac

echo "[artifact-size] wrote $SUMMARY"
