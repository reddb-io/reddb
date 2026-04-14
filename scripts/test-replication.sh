#!/usr/bin/env bash
set -euo pipefail

COMPOSE_FILE="${1:-examples/docker-compose.replica.yml}"
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "=== RedDB Replication Test ==="
echo "  compose file: ${COMPOSE_FILE}"
echo "  project dir:  ${PROJECT_DIR}"
echo ""

cd "${PROJECT_DIR}"

cleanup() {
    echo ""
    echo "5. Tearing down..."
    docker compose -f "${COMPOSE_FILE}" down -v 2>/dev/null || true
}
trap cleanup EXIT

echo "1. Building and starting primary + replica(s)..."
docker compose -f "${COMPOSE_FILE}" up -d --build

echo ""
echo "   Waiting for primary to become healthy..."
for i in $(seq 1 30); do
    STATUS=$(docker inspect --format='{{.State.Health.Status}}' reddb-primary 2>/dev/null || echo "starting")
    if [ "${STATUS}" = "healthy" ]; then
        echo "   Primary is healthy (took ~${i}s)"
        break
    fi
    if [ "${i}" -eq 30 ]; then
        echo "   ERROR: Primary did not become healthy within 30s"
        docker compose -f "${COMPOSE_FILE}" logs primary
        exit 1
    fi
    sleep 1
done

echo ""
echo "2. Checking container status..."
docker compose -f "${COMPOSE_FILE}" ps

echo ""
echo "3. Primary logs (last 10 lines):"
docker compose -f "${COMPOSE_FILE}" logs primary --tail 10

echo ""
echo "4. Replica logs (last 10 lines):"
docker compose -f "${COMPOSE_FILE}" logs replica --tail 10 2>/dev/null \
    || docker compose -f "${COMPOSE_FILE}" logs replica-1 --tail 10 2>/dev/null \
    || echo "   (no replica service found)"

echo ""
echo "=== Test Complete ==="
echo ""
echo "Port mapping:"
echo "  Primary gRPC : localhost:50051"
echo "  Replica gRPC : localhost:50052"
echo ""
echo "To interact manually:"
echo "  grpcurl -plaintext localhost:50051 list"
echo "  grpcurl -plaintext localhost:50052 list"
