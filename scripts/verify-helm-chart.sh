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

# Render must fail (helm template returns non-zero) AND the captured stderr must
# contain the expected operator-facing pattern. Used to prove a fail-closed gate
# rejects an unsupported value combination with a precise message.
assert_template_fails() {
  local pattern="$1"
  shift
  local err
  if err="$(helm template smoke "${CHART}" --namespace reddb-test "$@" 2>&1 1>/dev/null)"; then
    echo "expected 'helm template $*' to fail, but it succeeded" >&2
    exit 1
  fi
  if ! printf '%s' "${err}" | grep -qE "$pattern"; then
    echo "expected failure message to match: ${pattern}" >&2
    echo "got: ${err}" >&2
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

# --- Cluster bootstrap contract (issue #1234) ---------------------------------
# Symmetric cluster members are non-owners: they must never receive auth
# bootstrap credentials, because a member that is not the reserved global system
# range owner must not mutate global auth state (ADR 0058 / first-boot.md).
assert_grep 'value: "cluster-member"' "${TMP}/cluster.yaml"
assert_not_grep 'name: REDDB_PRESET' "${TMP}/cluster.yaml"
assert_not_grep 'name: REDDB_USERNAME' "${TMP}/cluster.yaml"
assert_not_grep 'name: REDDB_PASSWORD' "${TMP}/cluster.yaml"
assert_not_grep 'name: REDDB_BOOTSTRAP_MANIFEST' "${TMP}/cluster.yaml"

# Chart-managed auth bootstrap stays fail-closed in cluster mode until the
# runtime reserved-range owner path lands (PRD #1227): rendering must fail with
# the authority-aware message rather than emit a member that boots into a
# CrashLoopBackOff on the runtime fail-closed seam.
assert_template_fails 'reserved global system range owner' \
  -f "${CHART}/ci/cluster-values.yaml" --set auth.enabled=true

# Certificate handling: a cluster member may still receive the vault certificate
# (to unseal an already-bootstrapped store), even though it never bootstraps.
helm template smoke "${CHART}" --namespace reddb-test \
  -f "${CHART}/ci/cluster-values.yaml" \
  --set auth.vault.enabled=true \
  --set auth.vault.certificate.existingSecret=reddb-cluster-cert \
  >"${TMP}/cluster-vault.yaml"
assert_grep 'name: REDDB_CERTIFICATE' "${TMP}/cluster-vault.yaml"
assert_not_grep 'name: REDDB_PASSWORD' "${TMP}/cluster-vault.yaml"

# --- Compose cluster bootstrap contract (issue #1234) -------------------------
# The Compose cluster example mirrors the Helm contract: symmetric members carry
# no bootstrap credentials, and the vault example documents the supported
# single-owner bootstrap path (`red bootstrap` against the real volume).
COMPOSE_CLUSTER="${ROOT}/examples/docker-compose.cluster.yml"
COMPOSE_VAULT="${ROOT}/examples/docker-compose.vault.yml"
# Match the Compose env-assignment form (`KEY:`) so the contract check ignores
# the explanatory header comment, which names these vars in prose.
assert_grep 'REDDB_NODE_ROLE: cluster-member' "${COMPOSE_CLUSTER}"
assert_not_grep 'REDDB_USERNAME:' "${COMPOSE_CLUSTER}"
assert_not_grep 'REDDB_PASSWORD:' "${COMPOSE_CLUSTER}"
assert_not_grep 'REDDB_PRESET:' "${COMPOSE_CLUSTER}"
assert_not_grep 'REDDB_BOOTSTRAP_MANIFEST:' "${COMPOSE_CLUSTER}"
assert_grep 'print-certificate' "${COMPOSE_VAULT}"
assert_grep 'Bootstrap the same Docker volume the server will use' "${COMPOSE_VAULT}"

echo "helm chart topology verification passed"
