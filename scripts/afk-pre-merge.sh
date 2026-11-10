#!/usr/bin/env bash
# scripts/afk-pre-merge.sh — AFK pre_merge hook (drift-guard)
#
# Hard gate before merge. Wired from `.red/config.yaml` under
# `plugins.dev.afk.hooks.pre_merge`. If any check fails, the AFK
# routes the issue to `ready-for-human` with `blocked:validation`
# and the offending lines end up in the envelope.
#
# Contract (see ADR 0062, and the hook table in the dev plugin's
# `afk/docs/CONFIG.md` which is the authority on the stdio protocol):
#   - Input:  RED_AFK_WORKTREE_PATH env + (optional) unified diff on
#             stdin. The harness guarantees the worktree is on the
#             worker branch with the attempt's commits applied.
#   - Output: report goes to STDERR. Stdout is the engine's structured
#             channel: "empty stdout -> context unchanged; JSON object
#             -> AFK replaces the mutable slice; non-JSON stdout is a
#             parse failure". We do NOT mutate context here, so stdout
#             must stay empty. Printing the report to stdout aborted
#             the merge and mislabelled it `merge-conflict` — it cost
#             10 workers a full attempt each on 2026-08-13
#             (red-skills#3862). Keep every report line `>&2`.
#   - Exit:   0 = merge allowed; non-zero = merge BLOCKED.
#             `pre_*` non-zero aborts the step, which is the gate.
#
# Checks (all run from the worker's branch vs the merge base):
#   1. Scope drift       — more than 25 files changed → block.
#   2. Protected paths   — touches to /docs/conformance/,
#                          /testdata/conformance/, contract matrix
#                          scripts, or .red/adr/ require a human
#                          (CODEOWNERS already enforces; this is a
#                          faster fail before CI burns the round).
#   3. New .unwrap()     — added `.unwrap()` in a file NOT on the
#                          existing serialization whitelist is a
#                          ratchet violation (ADR 0056 + 0010).
#   4. New dependency    — added `name = "..."` / `name = { ... }`
#                          entry in Cargo.toml without a `# track:
#                          issue-XXXX` comment on the same line →
#                          block. Forces the tracking issue discipline.
#
# The scope threshold is deliberately conservative — a drift-guard
# that fires too often gets disabled. Tune UP only after looking at a
# month of false positives.

set -eu
set -o pipefail

REPO="${RED_AFK_WORKTREE_PATH:-${RED_AFK_REPO_PATH:-$PWD}}"
if [ ! -d "$REPO" ]; then
    printf 'pre_merge: worktree path %s does not exist\n' "$REPO" >&2
    exit 1
fi

cd "$REPO"

# Pull the diff base. The harness pins this via the lock/pin/main
# precedence from ADR 0033. Honour the env override when present.
BASE="${RED_AFK_MERGE_BASE:-origin/main}"
HEAD="${RED_AFK_MERGE_HEAD:-HEAD}"

# Refuse to run if we can't compute the diff — that means the harness
# handed us something we don't understand.
if ! git rev-parse --verify --quiet "$BASE" >/dev/null; then
    printf 'pre_merge: cannot resolve base ref %s\n' "$BASE" >&2
    exit 1
fi
if ! git rev-parse --verify --quiet "$HEAD" >/dev/null; then
    printf 'pre_merge: cannot resolve head ref %s\n' "$HEAD" >&2
    exit 1
fi

# Collect diff metadata once.
mapfile -t CHANGED_FILES < <(git diff --name-only "$BASE"..."$HEAD")
NUM_CHANGED=${#CHANGED_FILES[@]}

failed=0

check() {
    local title="$1"
    local detail="$2"
    printf '[FAIL] %s\n%s\n\n' "$title" "$detail" >&2
    failed=1
}

# 1. Scope drift.
SCOPE_LIMIT=25
if [ "$NUM_CHANGED" -gt "$SCOPE_LIMIT" ]; then
    check "scope-drift" "$NUM_CHANGED files changed (limit: $SCOPE_LIMIT). Break the PR down."
fi

# 2. Protected paths — mirror CODEOWNERS at `.github/CODEOWNERS`. The
#    contract matrix is the release-blocking artifact; if the agent
#    wants to MODIFY it, a human must. New files (added ADRs, new
#    conformance fixtures) are legitimate flow — don't blanket-block
#    those. `--diff-filter=M` keeps just the Modified paths.
PROTECTED_RE='^(docs/conformance/|testdata/conformance/|\.red/adr/|scripts/check-contract-authorities\.mjs|scripts/verify-contract-matrix\.mjs|scripts/contract_matrix_contract\.test\.mjs|scripts/verify-deployment-mode-matrix\.mjs|scripts/deployment_mode_matrix_contract\.test\.mjs)'
if [ "$NUM_CHANGED" -gt 0 ]; then
    bad_paths=$(git diff --name-only --diff-filter=M "$BASE"..."$HEAD" \
        | grep -E "$PROTECTED_RE" || true)
    if [ -n "$bad_paths" ]; then
        check "protected-paths" "modified release-locked paths (CODEOWNERS will reject anyway):
$bad_paths"
    fi
fi

# 3. New `.unwrap()` outside the existing whitelist
#    (`scripts/lint-untyped-serialization-whitelist.txt`).
WHITELIST="scripts/lint-untyped-serialization-whitelist.txt"
if [ "$NUM_CHANGED" -gt 0 ]; then
    bad_unwraps=""
    while IFS= read -r f; do
        case "$f" in
            *.rs) : ;;
            *) continue ;;
        esac
        # Pull just added lines containing `.unwrap()`.
        while IFS= read -r line; do
            [ -z "$line" ] && continue
            # Whitelist match: same suffix-path rule as the existing
            # lint. We do a simple grep here — duplicates the logic
            # in `lint-no-untyped-serialization.sh` but on the DIFF.
            if [ -f "$WHITELIST" ] \
                && grep -qF "$f" "$WHITELIST"; then
                continue
            fi
            bad_unwraps+="  $f: $line"$'\n'
        done < <(git diff -U0 --diff-filter=ACMRT "$BASE"..."$HEAD" -- "$f" \
            | grep -E '^\+.*\.unwrap\(\)' \
            | sed -E 's/^\+//' || true)
    done < <(printf '%s\n' "${CHANGED_FILES[@]}")
    if [ -n "$bad_unwraps" ]; then
        check "new-unwrap" "added .unwrap() in non-allowlisted files (ratchet, ADR 0056):
$bad_unwraps"
    fi
fi

# 4. New dependency in Cargo.toml without `# track:` marker.
if printf '%s\n' "${CHANGED_FILES[@]}" | grep -qx 'Cargo.toml'; then
    bad_deps=$(git diff -U0 "$BASE"..."$HEAD" -- Cargo.toml \
        | awk '
            /^\+/ && !/^\+\+\+/ {
                line = substr($0, 2)
                # Cargo dep entry: `name = "..."` or `name = { ... }`
                if (line ~ /^[a-zA-Z0-9_-]+[[:space:]]*=/) {
                    # Same line must NOT contain a `# track:` marker.
                    if (line !~ /#[[:space:]]*track:/) {
                        printf("  %s\n", line)
                    }
                }
            }
        ' || true)
    if [ -n "$bad_deps" ]; then
        check "new-dep-no-track" "new dependency entries in Cargo.toml without '# track: issue-XXXX':
$bad_deps"
    fi
fi

if [ "$failed" -ne 0 ]; then
    printf '\npre_merge: BLOCKED. Route issue to ready-for-human.\n' >&2
    exit 1
fi

printf 'pre_merge: OK (%s files, base=%s head=%s)\n' \
    "$NUM_CHANGED" "$BASE" "$HEAD" >&2
exit 0
