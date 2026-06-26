#!/usr/bin/env bash
# Safe local fuzzing on a memory-constrained box. Builds each target
# (the cargo guard already memory-caps builds), then runs the fuzzer inside
# a systemd memory cgroup so a runaway parser allocation is OOM-killed in the
# cgroup — never the desktop — and libFuzzer's -malloc_limit_mb turns a
# huge single allocation into a reported crash (an actionable DoS finding)
# instead of a machine hang.
#
# Usage:
#   ./scripts/fuzz-local.sh                 # fuzz all targets
#   ./scripts/fuzz-local.sh sql_parser      # fuzz one target
#   FUZZ_TIME=30 ./scripts/fuzz-local.sh    # override per-target seconds
set -euo pipefail

TIME="${FUZZ_TIME:-60}"             # seconds per target
MEM_MAX="${FUZZ_MEM_MAX:-10G}"      # cgroup hard cap (box has ~14G; leaves headroom)
RSS_MB="${FUZZ_RSS_MB:-4096}"       # libFuzzer per-process RSS limit
MALLOC_MB="${FUZZ_MALLOC_MB:-2048}" # abort+report on a single huge malloc

ALL=(sql_parser migration_parser conn_string_parser query_with_params)
if [ "$#" -gt 0 ]; then
  TARGETS=("$@")
else
  TARGETS=("${ALL[@]}")
fi

# Pin nightly via RUSTUP_TOOLCHAIN rather than a `cargo +nightly` prefix: the
# `+toolchain` form is only understood by the rustup proxy, and some
# environments shadow that proxy with a wrapper (e.g. a memory-capping cargo
# guard) that doesn't forward `+nightly`. RUSTUP_TOOLCHAIN works through any
# such wrapper and is honoured by the nested `cargo build` that cargo-fuzz
# spawns — so a guard wrapper still memory-caps the heavy build.
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-nightly}"

# Use the rustup nightly cargo directly for the RUN phase so we don't nest a
# second systemd scope inside a cargo guard wrapper (the build below already
# runs guard-capped). Fall back to `cargo` if rustup can't resolve it.
REAL_CARGO="$(rustup which --toolchain nightly cargo 2>/dev/null || echo cargo)"

for t in "${TARGETS[@]}"; do
  echo ">>> fuzz $t  (${TIME}s, MemoryMax=$MEM_MAX, rss=${RSS_MB}MB, malloc=${MALLOC_MB}MB)"
  cargo fuzz build "$t"    # guard-capped build (nightly via RUSTUP_TOOLCHAIN)
  if command -v systemd-run >/dev/null 2>&1; then
    systemd-run --user --scope -q -p MemoryMax="$MEM_MAX" -p MemorySwapMax=0 -- \
      "$REAL_CARGO" fuzz run "$t" -- \
        -max_total_time="$TIME" -rss_limit_mb="$RSS_MB" -malloc_limit_mb="$MALLOC_MB"
  else
    echo "WARN: systemd-run not available — running with libFuzzer limits only (no cgroup)"
    "$REAL_CARGO" fuzz run "$t" -- \
      -max_total_time="$TIME" -rss_limit_mb="$RSS_MB" -malloc_limit_mb="$MALLOC_MB"
  fi
done
