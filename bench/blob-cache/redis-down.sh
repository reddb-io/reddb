#!/usr/bin/env bash
# Tear down the blob-cache bench Redis containers.
# The AOF volume is preserved for workload-7 reruns.
# Pass --wipe-aof to also remove the volume.
set -euo pipefail

docker stop reddb-bench-redis-no-persist 2>/dev/null || true
docker stop reddb-bench-redis-aof-everysec 2>/dev/null || true

if [[ "${1:-}" == "--wipe-aof" ]]; then
  docker volume rm reddb-bench-redis-aof 2>/dev/null || true
  echo "AOF volume wiped."
else
  echo "AOF volume preserved. Pass --wipe-aof to remove it."
fi
echo "Redis containers stopped."
