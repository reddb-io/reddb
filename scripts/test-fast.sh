#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BASE_CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
if [ "${REDDB_FAST_SHARED_TARGET:-0}" = "1" ]; then
  CARGO_TARGET_DIR="$BASE_CARGO_TARGET_DIR"
else
  CARGO_TARGET_DIR="${REDDB_FAST_TARGET_DIR:-${BASE_CARGO_TARGET_DIR%/}/test-fast}"
fi
export CARGO_TARGET_DIR
mkdir -p "$CARGO_TARGET_DIR"

if [ -z "${REDDB_CARGO_BIN:-}" ] && command -v rustup >/dev/null 2>&1; then
  REDDB_CARGO_BIN="$(rustup which cargo)"
  export REDDB_CARGO_BIN
fi
if [ -z "${RUSTC:-}" ] && command -v rustup >/dev/null 2>&1; then
  RUSTC="$(rustup which rustc)"
  export RUSTC
fi

LOCK_FILE="$CARGO_TARGET_DIR/.reddb-test-fast.lock"
exec 9>"$LOCK_FILE"
if command -v flock >/dev/null 2>&1; then
  if ! flock -n 9; then
    echo "test-fast: another test-fast run is already using $CARGO_TARGET_DIR" >&2
    echo "test-fast: wait for it to finish or choose a different CARGO_TARGET_DIR" >&2
    exit 75
  fi
fi

FAST_TESTS=(
  audit_structured
  auth_tenant_isolation
  cross_binary_smoke
  e2e_documents_first_class_crud
  e2e_ddl_drop_foundation
  e2e_red_queue_pending
  e2e_issue_535_red_queues_virtual_table
  integration_queue_timeseries
  e2e_config_secret_ref
  e2e_evidence_export
  e2e_events_backfill
  e2e_issue_551_documents_sql_json_access
  e2e_issue_555_documents_sql_aggregates
  e2e_issue_751_json_patch_path_helpers
  e2e_fold_dwb_into_wal_policy
)

if [ -n "${REDDB_FAST_TESTS:-}" ]; then
  # Space-separated cargo integration test target names. Overrides the
  # default curated list for targeted runner diagnostics.
  # shellcheck disable=SC2206
  FAST_TESTS=(${REDDB_FAST_TESTS})
fi

if [ -n "${REDDB_FAST_EXTRA_TESTS:-}" ]; then
  # Space-separated cargo integration test target names.
  # shellcheck disable=SC2206
  EXTRA_TESTS=(${REDDB_FAST_EXTRA_TESTS})
  FAST_TESTS+=("${EXTRA_TESTS[@]}")
fi

run_step() {
  local label="$1"
  shift
  local start end elapsed safe_label log_file status
  safe_label="${label//[^A-Za-z0-9_.-]/_}"
  log_file="$CARGO_TARGET_DIR/test-fast-logs/${safe_label}.log"
  mkdir -p "$CARGO_TARGET_DIR/test-fast-logs"

  if command -v lsof >/dev/null 2>&1 && [ -e "$CARGO_TARGET_DIR/debug/.cargo-lock" ]; then
    local holders
    holders="$(lsof "$CARGO_TARGET_DIR/debug/.cargo-lock" 2>/dev/null || true)"
    if [ -n "$holders" ]; then
      echo "test-fast: cargo target is already busy before '$label'" >&2
      echo "$holders" >&2
      exit 75
    fi
  fi

  start="$(date +%s)"
  echo "[test-fast] $label"
  set +e
  if [ "${REDDB_FAST_VERBOSE:-0}" = "1" ]; then
    if command -v timeout >/dev/null 2>&1; then
      timeout "${REDDB_FAST_STEP_TIMEOUT:-300s}" "$@"
    else
      "$@"
    fi
  else
    if command -v timeout >/dev/null 2>&1; then
      timeout "${REDDB_FAST_STEP_TIMEOUT:-300s}" "$@" >"$log_file" 2>&1
    else
      "$@" >"$log_file" 2>&1
    fi
  fi
  status=$?
  set -e
  if [ "$status" -ne 0 ]; then
    echo "[test-fast] $label failed with status $status" >&2
    if [ "${REDDB_FAST_VERBOSE:-0}" != "1" ] && [ -f "$log_file" ]; then
      cat "$log_file" >&2
    fi
    if [ "$status" -eq 124 ]; then
      echo "[test-fast] $label exceeded REDDB_FAST_STEP_TIMEOUT=${REDDB_FAST_STEP_TIMEOUT:-300s}" >&2
      if command -v lsof >/dev/null 2>&1 && [ -e "$CARGO_TARGET_DIR/debug/.cargo-lock" ]; then
        lsof "$CARGO_TARGET_DIR/debug/.cargo-lock" >&2 || true
      fi
    fi
    exit "$status"
  fi
  end="$(date +%s)"
  elapsed=$((end - start))
  echo "[test-fast] $label ok (${elapsed}s)"
}

total_start="$(date +%s)"

run_step "unit+bin" ./scripts/cargo-fast.sh test --quiet --locked --lib --bins

for test_name in "${FAST_TESTS[@]}"; do
  run_step "$test_name" ./scripts/cargo-fast.sh test --quiet --locked --test "$test_name"
done

total_end="$(date +%s)"
echo "[test-fast] all ok ($((total_end - total_start))s)"
