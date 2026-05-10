#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${KV_BENCH_OUT:-$ROOT/bench/results/kv-latest.json}"

mkdir -p "$(dirname "$OUT")"
python3 "$ROOT/bench/kv/run.py" "$OUT"
