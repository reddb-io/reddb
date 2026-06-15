#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHART="${ROOT}/charts/reddb"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

require() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

assert_grep() {
  local pattern="$1"
  local file="$2"
  if ! grep -qE "$pattern" "$file"; then
    echo "expected pattern not found in ${file}: ${pattern}" >&2
    exit 1
  fi
}

assert_not_grep() {
  local pattern="$1"
  local file="$2"
  if grep -qE "$pattern" "$file"; then
    echo "unexpected pattern found in ${file}: ${pattern}" >&2
    exit 1
  fi
}

require helm

helm lint "${CHART}"
helm lint "${CHART}" -f "${CHART}/ci/standalone-values.yaml"
helm lint "${CHART}" -f "${CHART}/ci/primary-replica-values.yaml"
helm lint "${CHART}" -f "${CHART}/ci/serverless-values.yaml"
helm lint "${CHART}" -f "${CHART}/ci/cluster-values.yaml"

helm template smoke "${CHART}" --namespace reddb-test >"${TMP}/standalone.yaml"
helm template smoke "${CHART}" --namespace reddb-test \
  -f "${CHART}/ci/primary-replica-values.yaml" >"${TMP}/primary-replica.yaml"
helm template smoke "${CHART}" --namespace reddb-test \
  --set mode=primary-replica \
  --set replica.replicaCount=0 >"${TMP}/primary-replica-zero.yaml"
helm template smoke "${CHART}" --namespace reddb-test \
  -f "${CHART}/ci/serverless-values.yaml" >"${TMP}/serverless.yaml"
helm template smoke "${CHART}" --namespace reddb-test \
  -f "${CHART}/ci/cluster-values.yaml" >"${TMP}/cluster.yaml"

assert_grep 'value: "embedded"' "${TMP}/standalone.yaml"
assert_grep 'value: "primary-replica-production-ha"' "${TMP}/primary-replica.yaml"
assert_grep 'name: smoke-reddb-replica' "${TMP}/primary-replica.yaml"
assert_not_grep 'app.kubernetes.io/component: replica' "${TMP}/primary-replica-zero.yaml"

assert_grep 'kind: ConfigMap' "${TMP}/serverless.yaml"
assert_grep 'name: REDDB_CONFIG_FILE' "${TMP}/serverless.yaml"
assert_grep 'value: "serverless"' "${TMP}/serverless.yaml"
assert_grep 'name: RED_BACKEND' "${TMP}/serverless.yaml"
assert_grep 'name: RED_LEASE_REQUIRED' "${TMP}/serverless.yaml"

assert_grep 'name: smoke-reddb-cluster' "${TMP}/cluster.yaml"
assert_grep 'name: REDDB_CLUSTER_PEERS' "${TMP}/cluster.yaml"
assert_grep 'value: "cluster"' "${TMP}/cluster.yaml"
assert_grep 'name: RED_CLUSTER_HA_INTENT' "${TMP}/cluster.yaml"

echo "helm chart topology verification passed"
