#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${1:-replica}"
MODE="${2:-all}"

KEEP_UP="${KEEP_UP:-0}"
BUILD="${BUILD:-1}"

COMPOSE_FILE=""
TEST_COMPOSE_DIR="testdata/compose"
PRIMARY_HTTP_URL=""
PRIMARY_GRPC_ADDR=""
REPLICA_HTTP_URL="${REDDB_TEST_REPLICA_HTTP_URL:-}"
REPLICA_GRPC_ADDR="${REDDB_TEST_REPLICA_GRPC_ADDR:-}"
SECONDARY_REPLICA_HTTP_URL="${REDDB_TEST_SECONDARY_REPLICA_HTTP_URL:-}"
SECONDARY_REPLICA_GRPC_ADDR="${REDDB_TEST_SECONDARY_REPLICA_GRPC_ADDR:-}"
MINIO_URL="${REDDB_TEST_MINIO_URL:-}"
HEALTH_URLS=()

usage() {
    cat <<'EOF'
Usage:
  scripts/test-environment.sh <profile> [mode]

Profiles:
  min | replica | full | remote | backup | pitr | serverless

Modes:
  all   - shell checks + Rust external-env tests (default)
  shell - only Docker/HTTP checks
  rust  - only Rust external-env tests (expects env already up)

Environment:
  KEEP_UP=1   keep the stack running after the script exits
  BUILD=0     skip docker compose --build on startup
EOF
}

http_status() {
    local method="$1"
    local url="$2"
    local body="${3:-}"
    if [ -n "${body}" ]; then
        curl -sS -o /tmp/reddb_http_body.$$ -w "%{http_code}" \
            -X "${method}" \
            -H "content-type: application/json" \
            --data "${body}" \
            "${url}"
    else
        curl -sS -o /tmp/reddb_http_body.$$ -w "%{http_code}" \
            -X "${method}" \
            "${url}"
    fi
}

assert_http_ok() {
    local method="$1"
    local url="$2"
    local body="${3:-}"
    local code
    code="$(http_status "${method}" "${url}" "${body}")"
    if [ "${code}" -lt 200 ] || [ "${code}" -ge 300 ]; then
        echo "HTTP check failed: ${method} ${url} -> ${code}" >&2
        cat /tmp/reddb_http_body.$$ >&2 || true
        exit 1
    fi
}

assert_http_status_one_of() {
    local method="$1"
    local url="$2"
    local allowed="$3"
    local body="${4:-}"
    local code
    code="$(http_status "${method}" "${url}" "${body}")"
    case " ${allowed} " in
        *" ${code} "*) ;;
        *)
            echo "Unexpected HTTP status: ${method} ${url} -> ${code}; expected one of: ${allowed}" >&2
            cat /tmp/reddb_http_body.$$ >&2 || true
            exit 1
            ;;
    esac
}

wait_for_health() {
    local url="$1"
    local label="$2"
    local code=""
    for i in $(seq 1 90); do
        code="$(curl -sS -o /tmp/reddb_http_body.$$ -w "%{http_code}" "${url}" || true)"
        if [ "${code}" = "200" ]; then
            echo "  [ok] ${label} healthy after ~${i}s"
            return 0
        fi
        sleep 1
    done
    echo "Timed out waiting for ${label} at ${url}" >&2
    return 1
}

configure_profile() {
    case "${PROFILE}" in
        min)
            COMPOSE_FILE="${TEST_COMPOSE_DIR}/min.yml"
            PRIMARY_HTTP_URL="http://127.0.0.1:8080"
            PRIMARY_GRPC_ADDR="127.0.0.1:50051"
            HEALTH_URLS=("server|${PRIMARY_HTTP_URL}/health")
            ;;
        replica)
            COMPOSE_FILE="${TEST_COMPOSE_DIR}/replica.yml"
            PRIMARY_HTTP_URL="http://127.0.0.1:8080"
            PRIMARY_GRPC_ADDR="127.0.0.1:50051"
            REPLICA_HTTP_URL="http://127.0.0.1:8081"
            REPLICA_GRPC_ADDR="127.0.0.1:50052"
            HEALTH_URLS=("primary|${PRIMARY_HTTP_URL}/health" "replica|${REPLICA_HTTP_URL}/health")
            ;;
        full)
            COMPOSE_FILE="${TEST_COMPOSE_DIR}/full.yml"
            PRIMARY_HTTP_URL="http://127.0.0.1:8080"
            PRIMARY_GRPC_ADDR="127.0.0.1:50051"
            REPLICA_HTTP_URL="http://127.0.0.1:8081"
            REPLICA_GRPC_ADDR="127.0.0.1:50052"
            SECONDARY_REPLICA_HTTP_URL="http://127.0.0.1:8082"
            SECONDARY_REPLICA_GRPC_ADDR="127.0.0.1:50053"
            HEALTH_URLS=("primary|${PRIMARY_HTTP_URL}/health" "replica-1|${REPLICA_HTTP_URL}/health" "replica-2|${SECONDARY_REPLICA_HTTP_URL}/health")
            ;;
        remote)
            COMPOSE_FILE="${TEST_COMPOSE_DIR}/remote.yml"
            PRIMARY_HTTP_URL="http://127.0.0.1:8080"
            PRIMARY_GRPC_ADDR="127.0.0.1:50051"
            REPLICA_HTTP_URL="http://127.0.0.1:8081"
            REPLICA_GRPC_ADDR="127.0.0.1:50052"
            MINIO_URL="http://127.0.0.1:9000"
            HEALTH_URLS=("primary|${PRIMARY_HTTP_URL}/health" "replica|${REPLICA_HTTP_URL}/health")
            ;;
        backup)
            COMPOSE_FILE="${TEST_COMPOSE_DIR}/backup.yml"
            PRIMARY_HTTP_URL="http://127.0.0.1:8090"
            PRIMARY_GRPC_ADDR="127.0.0.1:50061"
            MINIO_URL="http://127.0.0.1:9010"
            HEALTH_URLS=("server|${PRIMARY_HTTP_URL}/health")
            ;;
        pitr)
            COMPOSE_FILE="${TEST_COMPOSE_DIR}/pitr.yml"
            PRIMARY_HTTP_URL="http://127.0.0.1:8100"
            PRIMARY_GRPC_ADDR="127.0.0.1:50071"
            MINIO_URL="http://127.0.0.1:9020"
            HEALTH_URLS=("primary|${PRIMARY_HTTP_URL}/health")
            ;;
        serverless)
            COMPOSE_FILE="${TEST_COMPOSE_DIR}/serverless.yml"
            PRIMARY_HTTP_URL="http://127.0.0.1:8110"
            PRIMARY_GRPC_ADDR="127.0.0.1:50081"
            MINIO_URL="http://127.0.0.1:9030"
            HEALTH_URLS=("serverless|${PRIMARY_HTTP_URL}/health")
            ;;
        -h|--help|help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown profile: ${PROFILE}" >&2
            usage >&2
            exit 1
            ;;
    esac
}

cleanup() {
    if [ "${KEEP_UP}" = "1" ] || [ "${MODE}" = "rust" ]; then
        return 0
    fi
    echo ""
    echo "==> Tearing down ${PROFILE}"
    docker compose -f "${COMPOSE_FILE}" down -v 2>/dev/null || true
}

start_stack() {
    local build_args=()
    if [ "${BUILD}" = "1" ]; then
        build_args+=(--build)
    fi
    echo "==> Starting ${PROFILE} with ${COMPOSE_FILE}"
    docker compose -f "${COMPOSE_FILE}" up -d "${build_args[@]}"
    for entry in "${HEALTH_URLS[@]}"; do
        local name="${entry%%|*}"
        local url="${entry#*|}"
        wait_for_health "${url}" "${name}"
    done
    docker compose -f "${COMPOSE_FILE}" ps
}

export_test_env() {
    export REDDB_TEST_PROFILE="${PROFILE}"
    export REDDB_TEST_PRIMARY_HTTP_URL="${PRIMARY_HTTP_URL}"
    export REDDB_TEST_PRIMARY_GRPC_ADDR="${PRIMARY_GRPC_ADDR}"
    export REDDB_TEST_REPLICA_HTTP_URL="${REPLICA_HTTP_URL}"
    export REDDB_TEST_REPLICA_GRPC_ADDR="${REPLICA_GRPC_ADDR}"
    export REDDB_TEST_SECONDARY_REPLICA_HTTP_URL="${SECONDARY_REPLICA_HTTP_URL}"
    export REDDB_TEST_SECONDARY_REPLICA_GRPC_ADDR="${SECONDARY_REPLICA_GRPC_ADDR}"
    export REDDB_TEST_MINIO_URL="${MINIO_URL}"
}

run_shell_checks() {
    echo "==> Running shell checks for ${PROFILE}"
    assert_http_ok GET "${PRIMARY_HTTP_URL}/health"
    assert_http_ok GET "${PRIMARY_HTTP_URL}/ready/query"

    case "${PROFILE}" in
        replica|full|remote)
            assert_http_ok GET "${PRIMARY_HTTP_URL}/replication/status"
            assert_http_ok GET "${REPLICA_HTTP_URL}/replication/status"
            if [ -n "${SECONDARY_REPLICA_HTTP_URL}" ]; then
                assert_http_ok GET "${SECONDARY_REPLICA_HTTP_URL}/replication/status"
            fi
            ;;
    esac

    case "${PROFILE}" in
        remote|backup|pitr|serverless)
            assert_http_ok GET "${PRIMARY_HTTP_URL}/backup/status"
            ;;
    esac

    case "${PROFILE}" in
        backup|pitr)
            assert_http_ok POST "${PRIMARY_HTTP_URL}/backup/trigger" "{}"
            assert_http_ok GET "${PRIMARY_HTTP_URL}/recovery/restore-points"
            ;;
    esac

    case "${PROFILE}" in
        serverless)
            assert_http_status_one_of GET "${PRIMARY_HTTP_URL}/ready/serverless" "200 503"
            assert_http_status_one_of GET "${PRIMARY_HTTP_URL}/ready/serverless/query" "200 503"
            assert_http_ok POST "${PRIMARY_HTTP_URL}/serverless/attach" "{}"
            assert_http_status_one_of POST "${PRIMARY_HTTP_URL}/serverless/warmup" "200 503" "{\"dry_run\":true}"
            ;;
    esac

    if [ -n "${MINIO_URL}" ]; then
        assert_http_status_one_of GET "${MINIO_URL}/minio/health/live" "200 403"
    fi
}

run_rust_tests() {
    echo "==> Running Rust external-environment tests for ${PROFILE}"
    cargo test --test integration_external_env -- --ignored --nocapture
}

main() {
    configure_profile
    trap cleanup EXIT
    cd "${PROJECT_DIR}"
    export_test_env

    case "${MODE}" in
        all)
            start_stack
            run_shell_checks
            run_rust_tests
            ;;
        shell)
            start_stack
            run_shell_checks
            ;;
        rust)
            run_rust_tests
            ;;
        *)
            echo "Unknown mode: ${MODE}" >&2
            usage >&2
            exit 1
            ;;
    esac
}

main "$@"
