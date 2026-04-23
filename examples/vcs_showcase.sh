#!/usr/bin/env bash
# RedDB "Git for Data" showcase.
#
# Runs a complete feature-branch flow against an in-memory database
# using the `red vcs` CLI. Prints hash / ref / merge-outcome at
# every step so you can read along.
#
# Usage:
#   cargo build --release --bin red
#   ./examples/vcs_showcase.sh
#
# The script never talks to the network — no REST server needed.
# For the REST variant see docs/guides/git-for-data.md.

set -euo pipefail

DB=$(mktemp -u /tmp/reddb-vcs-demo-XXXXXX.rdb)
RED="${RED:-./target/release/red}"

if [[ ! -x "$RED" ]]; then
    echo "build the binary first: cargo build --release --bin red" >&2
    exit 1
fi

trap 'rm -f "$DB"' EXIT

shared="--path $DB --author alice --email alice@example.com"

step() {
    echo
    echo "=== $* ==="
}

dump_log() {
    $RED vcs log --path "$DB" --limit 20 --json | jq -r \
        '.data[] | "\(.hash[0:12])  h=\(.height)  \(.message)"'
}

step "0. opt-in: create a versioned collection for this demo"
$RED vcs versioned on demo_users --path "$DB" | sed 's/^/  /'
$RED vcs versioned list --path "$DB" | sed 's/^/  /'

step "1. seed — initial commit"
$RED vcs commit "seed" $shared | sed 's/^/  /'

step "2. branch off + commit on feature-x"
$RED vcs branch feature-x --path "$DB" | sed 's/^/  /'
$RED vcs checkout feature-x --path "$DB" | sed 's/^/  /'
$RED vcs commit "feat: A" $shared | sed 's/^/  /'
$RED vcs commit "feat: B" $shared | sed 's/^/  /'

step "3. back to main, diverge"
$RED vcs checkout main --path "$DB" | sed 's/^/  /'
$RED vcs commit "main: hotfix" $shared | sed 's/^/  /'

step "4. log on feature-x"
$RED vcs checkout feature-x --path "$DB" >/dev/null
dump_log

step "5. log on main"
$RED vcs checkout main --path "$DB" >/dev/null
dump_log

step "6. LCA(main, feature-x)"
$RED vcs lca main feature-x --path "$DB" | sed 's/^/  /'

step "7. non-fast-forward merge"
$RED vcs merge feature-x $shared | sed 's/^/  /'

step "8. log after merge (main has the merge commit)"
dump_log

step "9. tag the release"
$RED vcs tag v1.0 main --path "$DB" | sed 's/^/  /'
$RED vcs tags --path "$DB" | sed 's/^/  /'

step "10. resolve short prefix + ref"
HEAD_HASH=$($RED vcs resolve main --path "$DB" --json | jq -r .data.hash)
echo "  main     -> $HEAD_HASH"
echo "  ${HEAD_HASH:0:10} -> $($RED vcs resolve "${HEAD_HASH:0:10}" --path "$DB" --json | jq -r .data.hash)"
echo "  v1.0     -> $($RED vcs resolve v1.0 --path "$DB" --json | jq -r .data.hash)"

step "11. fast-forward scenario"
$RED vcs branch ff-demo --path "$DB" | sed 's/^/  /'
$RED vcs checkout ff-demo --path "$DB" | sed 's/^/  /'
$RED vcs commit "trivial bump" $shared | sed 's/^/  /'
$RED vcs checkout main --path "$DB" | sed 's/^/  /'
$RED vcs merge ff-demo $shared | sed 's/^/  /'

step "12. reset --soft rewinds the branch but not the history"
OLD_HASH=$($RED vcs log --path "$DB" --limit 20 --json | jq -r '.data[-2].hash')
echo "  resetting main to $OLD_HASH"
$RED vcs reset "$OLD_HASH" --path "$DB" | sed 's/^/  /' || true
$RED vcs status --path "$DB" | sed 's/^/  /'

step "13. final status"
$RED vcs status --path "$DB" | sed 's/^/  /'

echo
echo "Done.  Database: $DB  (will be removed on exit)"
