#!/usr/bin/env bash
# scripts/afk-pre-pick.sh — AFK pre_pick hook
#
# Filters and reorders the issue queue before the harness spends an
# attempt. Wired from `.red/config.yaml` under
# `plugins.dev.afk.hooks.pre_pick`.
#
# Contract (see ADR 0062):
#   - Input:  context JSON on stdin. Queue under `.candidates` (array of
#             { number, title, labels[] }). The hook MAY reorder or
#             remove entries; what it returns IS the new queue.
#   - Output: JSON on stdout. Empty stdout is treated as no-op (queue
#             unchanged).
#   - Exit:   0 = OK (queue accepted); non-zero in `pre_*` aborts the
#             pick, so this script MUST return 0 even when the queue is
#             empty, unless the harness itself is misconfigured.
#
# Filter rules (defense in depth, not gate):
#   1. Drop issues carrying `do-not-pick` or any `blocked:external`
#      label. `blocked:dependency` is KEPT (the harness handles those
#      via its own gate; mixing layers is what created the rot
#      ADR 0059 fixed).
#   2. Require at least one `size:*` AND one `priority:*` label — the
#      triage minimum per `.red/agents/triage-labels.md`.
#   3. Reorder by `priority:*` ascending (P0 first). Unlabelled issues
#      fall back to priority 9 (last).
#
# Failure mode: if `gh` or `jq` fails, log to stderr and return 0 with
# the queue untouched. A broken pre_pick must NEVER starve the AFK —
# that is the lesson of the admin-merge bypass (ADR 0059).

set -u
# No `set -e` on purpose — `gh` exits non-zero on transient API blips
# and we want to log + pass-through, not abort the harness.

# Read context from stdin (don't barf if empty / piped from /dev/null).
ctx=""
if [ ! -t 0 ]; then
    ctx=$(cat || true)
fi

# Pull the candidate list. Fall back to the raw context if jq parsing
# fails — better than crashing the harness on malformed input.
if ! candidates=$(printf '%s' "$ctx" | jq -c '.candidates // []' 2>/dev/null); then
    printf 'afk-pre-pick: could not parse .candidates; passing through\n' >&2
    printf '%s' "$ctx"
    exit 0
fi

if [ -z "$candidates" ] || [ "$candidates" = "null" ] || [ "$candidates" = "[]" ]; then
    # No queue handed in (or empty) — nothing to filter. Pass through.
    printf '%s' "$ctx"
    exit 0
fi

# Filter + reorder in one jq pass. Bind the candidate up-front so the
# downstream `select(...)` chains operate on it, not on the lowercased
# labels array we extract for the filter predicates. Labels are
# lowercased for the contains/startswith checks; reddb labels are
# lowercase by convention but this protects against accidental drift.
printf '%s' "$ctx" | jq -c --argjson c "$candidates" '
    ($c
     | map(
         . as $cand
         | (.labels // [])
           | map(ascii_downcase) as $lbl
         | ($lbl | any(. == "do-not-pick")
                  or any(startswith("blocked:external"))) as $banned
         | ($lbl | any(startswith("size:")))     as $has_size
         | ($lbl | any(startswith("priority:"))) as $has_prio
         | select($banned | not)
         | select($has_size and $has_prio)
         | $cand
       )
     | sort_by(
         (.labels // [])
         | map(select(. | startswith("priority:")))[0]
         // "priority:9"
         | sub("^priority:"; "")
         | tonumber
       )
    ) as $filtered
    | .candidates = $filtered
'
