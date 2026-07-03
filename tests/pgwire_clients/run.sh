#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ -z "${RED_BIN:-}" ]]; then
  TARGET_DIR="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
  RED_BIN="${TARGET_DIR}/debug/red"
fi
PG_PORT="${PG_PORT:-55432}"
PROXY_PORT="${PROXY_PORT:-55433}"
AI_PORT="${AI_PORT:-55436}"
LOG_FILE="${LOG_FILE:-$(mktemp /tmp/reddb-pgwire360-log.XXXXXX)}"
DB_PATH="${DB_PATH:-$(mktemp /tmp/reddb-pgwire360-db.XXXXXX).rdb}"
PSYCOPG_IMAGE="${PSYCOPG_IMAGE:-python:3.12-slim@sha256:423ed6ab25b1921a477529254bfeeabf5855151dc2c3141699a1bfc852199fbf}"
PGX_IMAGE="${PGX_IMAGE:-golang:1.25@sha256:f188e8c16ea47a8b22d2bdcf6d9bcd07b63ea7876c199749c07bf31e0ab33bad}"
PGJDBC_IMAGE="${PGJDBC_IMAGE:-maven:3.9-eclipse-temurin-17@sha256:1ed5d1f54416b706707b4f3238f63a20bb06aab27c6d240090a2bb9ad895ed45}"

cleanup() {
  if [[ -n "${PROXY_PID:-}" ]]; then kill "$PROXY_PID" 2>/dev/null || true; fi
  if [[ -n "${SERVER_PID:-}" ]]; then kill "$SERVER_PID" 2>/dev/null || true; fi
  if [[ -n "${AI_PID:-}" ]]; then kill "$AI_PID" 2>/dev/null || true; fi
}
trap cleanup EXIT

python3 "$ROOT/tests/pgwire_clients/mock_ai.py" \
  --listen "127.0.0.1:${AI_PORT}" &
AI_PID=$!

REDDB_AI_PROVIDER=openai \
REDDB_OPENAI_API_KEY=test-key \
REDDB_OPENAI_API_BASE="http://127.0.0.1:${AI_PORT}/v1" \
REDDB_OPENAI_PROMPT_MODEL=mock-chat \
"$RED_BIN" server --pg-bind "127.0.0.1:${PG_PORT}" --path "$DB_PATH" --no-log-file &
SERVER_PID=$!

python3 "$ROOT/tests/pgwire_clients/proxy.py" \
  --listen "127.0.0.1:${PROXY_PORT}" \
  --target "127.0.0.1:${PG_PORT}" \
  --log "$LOG_FILE" &
PROXY_PID=$!

python3 - <<PY
import socket, time
for port in (${AI_PORT}, ${PG_PORT}, ${PROXY_PORT}):
    deadline = time.time() + 15
    while True:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                break
        except OSError:
            if time.time() > deadline:
                raise
            time.sleep(0.1)
PY

run_client() {
  local name="$1"
  local image="$2"
  local command="$3"

  echo "::group::pgwire client: ${name}"
  echo "Running ${name} with ${image}"
  if ! docker run --rm --network host \
    -e PGPORT="$PROXY_PORT" \
    -v "$ROOT/tests/pgwire_clients:/clients:ro" \
    -w /clients \
    "$image" \
    sh -lc "$command"; then
    echo "::endgroup::"
    echo "FAIL: pgwire client ${name} failed"
    return 1
  fi
  echo "::endgroup::"
}

run_client "psycopg" "$PSYCOPG_IMAGE" 'pip install -q "psycopg[binary]" && python psycopg_client.py'
run_client "pgx" "$PGX_IMAGE" 'export PATH=/usr/local/go/bin:$PATH; go mod download && go run pgx_client.go'
run_client "pgjdbc" "$PGJDBC_IMAGE" 'mkdir -p /tmp/pgwire360-classes && mvn -q dependency:copy-dependencies -DincludeScope=runtime -DoutputDirectory=/tmp/pgwire360-deps && javac -d /tmp/pgwire360-classes -cp "/tmp/pgwire360-deps/*" JdbcClient.java && java -cp "/tmp/pgwire360-classes:/tmp/pgwire360-deps/*" JdbcClient'

python3 "$ROOT/tests/pgwire_clients/assert_extended.py" "$LOG_FILE"
