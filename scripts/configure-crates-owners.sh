#!/usr/bin/env bash
set -euo pipefail

TEAM="${1:-github:reddb-io:crates-owners}"
CRATES=(
  reddb
  reddb-client
  reddb-client-connector
  reddb-grpc-proto
  reddb-server
  reddb-wire
)

cat <<EOF
Configuring crates.io team owner:
  team: ${TEAM}

Requirements:
  - You are logged in to crates.io with a token that owns each existing crate.
  - Your crates.io GitHub OAuth grant has read:org.
  - GitHub org reddb-io allows the crates.io OAuth app.

EOF

for crate in "${CRATES[@]}"; do
  echo "==> ${crate}"
  if cargo owner --list "${crate}" >/tmp/reddb-crate-owners.$$ 2>/tmp/reddb-crate-owners.err.$$; then
    if grep -q "${TEAM}" /tmp/reddb-crate-owners.$$; then
      echo "    already has ${TEAM}"
    else
      cargo owner --add "${TEAM}" "${crate}"
    fi
  elif grep -q "status 404 Not Found" /tmp/reddb-crate-owners.err.$$; then
    echo "    crate does not exist yet; add ${TEAM} immediately after first publish"
  else
    cat /tmp/reddb-crate-owners.err.$$ >&2
    rm -f /tmp/reddb-crate-owners.$$ /tmp/reddb-crate-owners.err.$$
    exit 1
  fi
  rm -f /tmp/reddb-crate-owners.$$ /tmp/reddb-crate-owners.err.$$
done
