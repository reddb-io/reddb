#!/usr/bin/env bash
# scripts/afk-pre-attempt.sh — AFK pre_attempt hook
#
# Injects a standard house-style + boundary-discipline reminder into
# every attempt's prompt. Wired from `.red/config.yaml` under
# `plugins.dev.afk.hooks.pre_attempt`. Cheaper than `--request -r` on
# every /afk invocation: set once, applies to every attempt on the run.
#
# Contract (see ADR 0062):
#   - Input:  context JSON on stdin (mutable slice under `.attempt`).
#   - Output: JSON on stdout with the mutated context. Empty stdout is
#             a no-op.
#   - Exit:   0 = OK (return the mutated context even if unchanged).
#             Non-zero in `pre_*` aborts the attempt — DO NOT use for
#             gating, only for context mutation.
#
# What we inject:
#   - House style reference (STYLE.md + ADR 0056).
#   - Boundary discipline reminder (the 6 lint categories + the
#     pre_merge drift-guard that will gate the attempt's PR).
#   - "Don't bypass the merge gate" — explicit pointer to ADR 0059 so
#     the agent doesn't try the admin-merge path that no longer works.
#
# `--request -r "..."` STILL wins (it's appended after this), so users
# retain per-run overrides.

set -u
# No `set -e`: a broken pre_attempt must not abort the harness.

# Read context from stdin (don't barf if empty / piped from /dev/null).
ctx=""
if [ ! -t 0 ]; then
    ctx=$(cat || true)
fi

# Locate the repo root from this script's path so the reminder can
# reference real, in-repo files.
REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

INSTRUCTION="Follow .red/CONTEXT.md and STYLE.md (TigerStyle subset per ADR 0056). \
Before committing, run \`scripts/lint-no-untyped-serialization.sh\` and \`cargo fmt --all\` in your worktree. \
Do NOT introduce bare \`.unwrap()\` outside the whitelist — it is a ratchet violation. \
Do NOT edit \`docs/conformance/\`, \`testdata/conformance/\`, the contract matrix scripts, or \`.red/adr/\`; CODEOWNERS blocks these and the pre_merge drift-guard (ADR 0062) will reject the PR. \
Do NOT add a new Cargo dependency without a \`# track: issue-XXXX\` comment on the same line — the drift-guard requires a tracking issue. \
Do NOT use \`gh pr merge --admin\` to bypass the gate on \`main\` — ADR 0059 enables \`enforce_admins\`; the gate is binding. \
Prefer declarative success criteria over imperative steps (CLAUDE.md §4)."

# Mutate the attempt context. The shape `{ attempt: { extra_instructions: ... } }`
# is the same convention the harness accepts on `pre_worktree` per the
# AFK CONFIG reference (env-injection example). The harness drops the
# field on stdout if absent in input, so we always set it.
#
# Defensive: if `$ctx` is non-empty but malformed (jq can't parse),
# fall back to synthesising the minimum context. A broken pre_attempt
# must NOT abort the harness (ADR 0059 lesson).
if [ -z "$ctx" ] \
    || ! printf '%s' "$ctx" | jq -e . >/dev/null 2>&1; then
    # Either empty or malformed — synthesise the minimum.
    printf '{"attempt":{"extra_instructions":%s}}\n' \
        "$(printf '%s' "$INSTRUCTION" | jq -Rs .)"
else
    printf '%s' "$ctx" | jq -c \
        --arg instr "$INSTRUCTION" \
        '.attempt = (.attempt // {}) | .attempt.extra_instructions = $instr'
fi
