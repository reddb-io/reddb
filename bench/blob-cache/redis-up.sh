#!/usr/bin/env bash
# Bring up both pinned Redis 7.4 variants for the blob-cache bench suite.
# See redis-setup.md for full flag rationale.
set -euo pipefail

docker run -d --rm \
  --name reddb-bench-redis-no-persist \
  --platform linux/amd64 \
  -p 127.0.0.1:6379:6379 \
  redis:7.4 \
  redis-server \
    --save "" \
    --appendonly no \
    --maxmemory 1gb \
    --maxmemory-policy allkeys-lru

docker run -d --rm \
  --name reddb-bench-redis-aof-everysec \
  --platform linux/amd64 \
  -p 127.0.0.1:6380:6379 \
  -v reddb-bench-redis-aof:/data \
  redis:7.4 \
  redis-server \
    --save "" \
    --appendonly yes \
    --appendfsync everysec \
    --dir /data \
    --maxmemory 1gb \
    --maxmemory-policy allkeys-lru

echo "Waiting for Redis containers to be ready..."
until docker exec reddb-bench-redis-no-persist redis-cli -p 6379 ping 2>/dev/null | grep -q PONG; do sleep 0.5; done
until docker exec reddb-bench-redis-aof-everysec redis-cli -p 6379 ping 2>/dev/null | grep -q PONG; do sleep 0.5; done

echo "Redis variants ready:"
echo "  no-persist  → 127.0.0.1:6379  (REDIS_NO_PERSIST_ADDR)"
echo "  aof-everysec → 127.0.0.1:6380 (REDIS_AOF_ADDR)"
echo ""
echo "Export before running benches:"
echo "  export REDIS_NO_PERSIST_ADDR=127.0.0.1:6379"
echo "  export REDIS_AOF_ADDR=127.0.0.1:6380"
