#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BASE_CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}"
if [ -n "${REDDB_FAST_TARGET_DIR:-}" ]; then
  CARGO_TARGET_DIR="$REDDB_FAST_TARGET_DIR"
elif [ "${REDDB_FAST_SHARED_TARGET:-1}" = "1" ]; then
  CARGO_TARGET_DIR="$BASE_CARGO_TARGET_DIR"
else
  CARGO_TARGET_DIR="${REDDB_FAST_TARGET_DIR:-${BASE_CARGO_TARGET_DIR%/}/test-fast}"
fi
export CARGO_TARGET_DIR
mkdir -p "$CARGO_TARGET_DIR"

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
  grouped_materialized_views_events_config_audit:audit_structured::
  grouped_auth_iam_security:auth_tenant_isolation::
  grouped_surface_contracts:cross_binary_smoke::
  grouped_documents_kv_queues:e2e_documents_first_class_crud::
  grouped_sql_core:e2e_ddl_drop_foundation::
  grouped_documents_kv_queues:e2e_red_queue_pending::
  grouped_documents_kv_queues:e2e_issue_535_red_queues_virtual_table::
  grouped_documents_kv_queues:integration_queue_timeseries::
  grouped_auth_iam_security:e2e_config_secret_ref::
  grouped_materialized_views_events_config_audit:e2e_evidence_export::
  grouped_materialized_views_events_config_audit:e2e_events_backfill::
  grouped_documents_kv_queues:e2e_issue_551_documents_sql_json_access::
  grouped_documents_kv_queues:e2e_issue_555_documents_sql_aggregates::
  grouped_general_multimodel:e2e_issue_751_json_patch_path_helpers::
  grouped_chaos_drill_persistence:e2e_fold_dwb_into_wal_policy::
)

if [ -n "${REDDB_FAST_TESTS:-}" ]; then
  # Space-separated targets or target:filter entries. Overrides the
  # default curated list for targeted runner diagnostics.
  # shellcheck disable=SC2206
  FAST_TESTS=(${REDDB_FAST_TESTS})
fi

if [ -n "${REDDB_FAST_EXTRA_TESTS:-}" ]; then
  # Space-separated targets or target:filter entries.
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

run_step "unit+bin" ./scripts/cargo-fast.sh test --quiet --locked --workspace --lib --bins
run_step "red_client" ./scripts/cargo-fast.sh build --quiet --locked -p reddb-io-client --no-default-features --bin red_client

for test_entry in "${FAST_TESTS[@]}"; do
  test_name="${test_entry%%:*}"
  test_args=(test --quiet --locked --test "$test_name")
  if [[ "$test_entry" == *:* ]]; then
    test_args+=("${test_entry#*:}")
  fi
  run_step "$test_entry" ./scripts/cargo-fast.sh "${test_args[@]}"
done

total_end="$(date +%s)"
echo "[test-fast] all ok ($((total_end - total_start))s)"
