#!/usr/bin/env bash
set -euo pipefail

# =============================================================================
# RedDB Cluster Test Script
# Tests a primary (50051) + 2 replicas (50052, 50053)
# =============================================================================

PRIMARY="127.0.0.1:50051"
REPLICA1="127.0.0.1:50052"
REPLICA2="127.0.0.1:50053"

PASS=0
FAIL=0
TOTAL=0

check() {
    local desc="$1"
    local result="$2"
    TOTAL=$((TOTAL + 1))
    if [ "$result" = "0" ]; then
        PASS=$((PASS + 1))
        echo "  [PASS] $desc"
    else
        FAIL=$((FAIL + 1))
        echo "  [FAIL] $desc"
    fi
}

echo "============================================"
echo "  RedDB Cluster Integration Tests"
echo "============================================"
echo ""

# -------------------------------------------------------------------
# Test 1: Health checks
# -------------------------------------------------------------------
echo "--- 1. Health Checks ---"

for port in 50051 50052 50053; do
    name="primary"
    [ "$port" = "50052" ] && name="replica-1"
    [ "$port" = "50053" ] && name="replica-2"

    grpcurl -plaintext "127.0.0.1:$port" reddb.v1.RedDb/Health >/dev/null 2>&1
    check "$name ($port) is healthy" "$?"
done

# -------------------------------------------------------------------
# Test 2: Write to primary
# -------------------------------------------------------------------
echo ""
echo "--- 2. Write to Primary ---"

# Create a row
RESULT=$(grpcurl -plaintext -d '{
    "collection": "users",
    "payload": "{\"fields\": {\"name\": \"Alice\", \"age\": 30, \"role\": \"admin\"}}"
}' "$PRIMARY" reddb.v1.RedDb/CreateRow 2>&1)
echo "$RESULT" | grep -q "id"
check "Create row (Alice) on primary" "$?"

# Create another row
RESULT=$(grpcurl -plaintext -d '{
    "collection": "users",
    "payload": "{\"fields\": {\"name\": \"Bob\", \"age\": 25, \"role\": \"user\"}}"
}' "$PRIMARY" reddb.v1.RedDb/CreateRow 2>&1)
echo "$RESULT" | grep -q "id"
check "Create row (Bob) on primary" "$?"

# Create a node
RESULT=$(grpcurl -plaintext -d '{
    "collection": "hosts",
    "payload": "{\"label\": \"web-server\", \"node_type\": \"Host\", \"properties\": {\"ip\": \"192.168.1.1\", \"os\": \"Linux\"}}"
}' "$PRIMARY" reddb.v1.RedDb/CreateNode 2>&1)
echo "$RESULT" | grep -q "id"
check "Create node (web-server) on primary" "$?"

# Create a vector
RESULT=$(grpcurl -plaintext -d '{
    "collection": "embeddings",
    "payload": "{\"dense\": [0.1, 0.2, 0.3, 0.4, 0.5], \"content\": \"test vector\"}"
}' "$PRIMARY" reddb.v1.RedDb/CreateVector 2>&1)
echo "$RESULT" | grep -q "id"
check "Create vector on primary" "$?"

# -------------------------------------------------------------------
# Test 3: Read from primary
# -------------------------------------------------------------------
echo ""
echo "--- 3. Read from Primary ---"

RESULT=$(grpcurl -plaintext -d '{
    "query": "SELECT * FROM users"
}' "$PRIMARY" reddb.v1.RedDb/Query 2>&1)
echo "$RESULT" | grep -q "Alice"
check "Query users on primary returns Alice" "$?"

echo "$RESULT" | grep -q "Bob"
check "Query users on primary returns Bob" "$?"

# -------------------------------------------------------------------
# Test 4: Read from replicas (verify they started)
# -------------------------------------------------------------------
echo ""
echo "--- 4. Read from Replicas ---"

# Replicas should be healthy and serve reads
RESULT=$(grpcurl -plaintext "$REPLICA1" reddb.v1.RedDb/Health 2>&1)
echo "$RESULT" | grep -q "healthy\|state"
check "Replica-1 health responds" "$?"

RESULT=$(grpcurl -plaintext "$REPLICA2" reddb.v1.RedDb/Health 2>&1)
echo "$RESULT" | grep -q "healthy\|state"
check "Replica-2 health responds" "$?"

# Replicas should be read-only (Stats should work)
RESULT=$(grpcurl -plaintext "$REPLICA1" reddb.v1.RedDb/Stats 2>&1)
check "Replica-1 Stats responds" "$?"

RESULT=$(grpcurl -plaintext "$REPLICA2" reddb.v1.RedDb/Stats 2>&1)
check "Replica-2 Stats responds" "$?"

# -------------------------------------------------------------------
# Test 5: Write rejection on replicas
# -------------------------------------------------------------------
echo ""
echo "--- 5. Write Rejection on Replicas ---"

RESULT=$(grpcurl -plaintext -d '{
    "collection": "users",
    "payload": "{\"fields\": {\"name\": \"Charlie\"}}"
}' "$REPLICA1" reddb.v1.RedDb/CreateRow 2>&1 || true)
echo "$RESULT" | grep -qi "read.only\|denied\|error\|permission\|PERMISSION_DENIED\|FAILED_PRECONDITION"
check "Replica-1 rejects writes" "$?"

RESULT=$(grpcurl -plaintext -d '{
    "collection": "users",
    "payload": "{\"fields\": {\"name\": \"Charlie\"}}"
}' "$REPLICA2" reddb.v1.RedDb/CreateRow 2>&1 || true)
echo "$RESULT" | grep -qi "read.only\|denied\|error\|permission\|PERMISSION_DENIED\|FAILED_PRECONDITION"
check "Replica-2 rejects writes" "$?"

# -------------------------------------------------------------------
# Test 6: Replication status
# -------------------------------------------------------------------
echo ""
echo "--- 6. Replication Status ---"

RESULT=$(grpcurl -plaintext "$PRIMARY" reddb.v1.RedDb/ReplicationStatus 2>&1)
echo "$RESULT" | grep -q "role\|lsn\|primary"
check "Primary replication status responds" "$?"
echo "  Primary status: $(echo "$RESULT" | tr '\n' ' ' | head -c 200)"

RESULT=$(grpcurl -plaintext "$REPLICA1" reddb.v1.RedDb/ReplicationStatus 2>&1)
check "Replica-1 replication status responds" "$?"

# -------------------------------------------------------------------
# Test 7: Universal query on primary
# -------------------------------------------------------------------
echo ""
echo "--- 7. Universal Query ---"

RESULT=$(grpcurl -plaintext -d '{
    "query": "SELECT * FROM any"
}' "$PRIMARY" reddb.v1.RedDb/Query 2>&1)
echo "$RESULT" | grep -q "Alice\|web-server\|test vector"
check "Universal query (FROM any) returns mixed types" "$?"

# -------------------------------------------------------------------
# Test 8: Explain query
# -------------------------------------------------------------------
echo ""
echo "--- 8. Query Explain ---"

RESULT=$(grpcurl -plaintext -d '{
    "query": "SELECT * FROM any"
}' "$PRIMARY" reddb.v1.RedDb/ExplainQuery 2>&1)
echo "$RESULT" | grep -q "is_universal\|plan\|cost"
check "Explain universal query shows plan" "$?"

# -------------------------------------------------------------------
# Summary
# -------------------------------------------------------------------
echo ""
echo "============================================"
echo "  Results: $PASS/$TOTAL passed, $FAIL failed"
echo "============================================"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
