#!/usr/bin/env bash
# Tests for scripts/report-parser-fuzz-crashes.sh dedup and zero-byte behaviour.
#
# Every case stubs `gh` (and `cargo`, so no real fuzzer runs) via a temp bin
# on PATH — the live tracker is never touched. Each stub appends one line per
# invocation to ${CALL_LOG} and captures any --body-file it receives, so the
# assertions can read exactly what the script tried to do.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${SCRIPT_DIR}/report-parser-fuzz-crashes.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

BIN="${WORK}/bin"
mkdir -p "${BIN}"

# --- gh stub --------------------------------------------------------------
# Behaviour is driven by env vars set per case:
#   STUB_ISSUE_NUMBER  -> what `gh issue list ... --jq` returns (open match, or empty)
#   STUB_ISSUE_BODY    -> what `gh issue view` returns (body + comments text)
# It logs `list`, `view`, `create`, `comment <n>` lines to ${CALL_LOG} and
# copies create/comment bodies to ${CREATE_BODY}/${COMMENT_BODY}.
cat > "${BIN}/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
sub="${1:-} ${2:-}"
case "${sub}" in
  "label create") exit 0 ;;
  "issue list")
    echo "list" >> "${CALL_LOG}"
    printf '%s' "${STUB_ISSUE_NUMBER:-}"
    ;;
  "issue view")
    echo "view ${3:-}" >> "${CALL_LOG}"
    printf '%s' "${STUB_ISSUE_BODY:-}"
    ;;
  "issue create")
    echo "create" >> "${CALL_LOG}"
    body=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--body-file" ]; then body="$2"; fi
      shift
    done
    [ -n "${body}" ] && cp "${body}" "${CREATE_BODY}"
    echo "https://github.com/reddb-io/reddb/issues/999"
    ;;
  "issue comment")
    num="${3:-}"
    echo "comment ${num}" >> "${CALL_LOG}"
    body=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--body-file" ]; then body="$2"; fi
      shift
    done
    [ -n "${body}" ] && cp "${body}" "${COMMENT_BODY}"
    ;;
  *) echo "unexpected gh call: $*" >&2; exit 3 ;;
esac
STUB
chmod +x "${BIN}/gh"

# cargo stub: swallow `cargo +nightly fuzz tmin ...` so no real fuzzer runs.
cat > "${BIN}/cargo" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
chmod +x "${BIN}/cargo"

export PATH="${BIN}:${PATH}"
export GITHUB_RUN_URL="https://github.com/reddb-io/reddb/actions/runs/12345"
export RUNNER_TEMP="${WORK}/runner-temp"
mkdir -p "${RUNNER_TEMP}"

FAILURES=0
fail() { echo "  FAIL: $*"; FAILURES=$((FAILURES + 1)); }
pass() { echo "  ok: $*"; }

# Fresh isolated working tree + one oom artifact whose basename is a content hash.
setup_case() {
  local dir="$1" artifact_basename="$2"
  rm -rf "${dir}"
  mkdir -p "${dir}/fuzz/artifacts/migration_parser"
  printf 'crashing-input-bytes' > "${dir}/fuzz/artifacts/migration_parser/${artifact_basename}"
  export CALL_LOG="${dir}/calls.log"
  export CREATE_BODY="${dir}/create-body.txt"
  export COMMENT_BODY="${dir}/comment-body.txt"
  : > "${CALL_LOG}"
}

count() { grep -c "^$1$" "${CALL_LOG}" 2>/dev/null || true; }

ART="oom-deadbeefcafe"

# --- Case 1: no matching open issue -> exactly one create -----------------
echo "case 1: no open match -> gh issue create once"
setup_case "${WORK}/c1" "${ART}"
unset STUB_ISSUE_BODY
STUB_ISSUE_NUMBER="" bash -c "cd '${WORK}/c1' && '${SCRIPT}' migration_parser" >/dev/null
[ "$(count create)" = "1" ] && pass "create called once" || fail "create count = $(count create) (want 1)"
[ "$(count 'comment .*')" = "0" ] || fail "comment should not be called"

# --- Case 2: open match already contains the basename -> silence ----------
echo "case 2: open match already records basename -> no create, no comment"
setup_case "${WORK}/c2" "${ART}"
export STUB_ISSUE_NUMBER="42"
export STUB_ISSUE_BODY="Existing issue body mentioning artifact ${ART} already."
bash -c "cd '${WORK}/c2' && '${SCRIPT}' migration_parser" >/dev/null
[ "$(count create)" = "0" ] && pass "create not called" || fail "create called (want 0)"
[ "$(grep -c '^comment ' "${CALL_LOG}" || true)" = "0" ] && pass "comment not called" || fail "comment called (want 0)"

# --- Case 3: open match missing the basename -> one comment on that issue --
echo "case 3: open match missing basename -> exactly one comment, no create"
setup_case "${WORK}/c3" "${ART}"
export STUB_ISSUE_NUMBER="42"
export STUB_ISSUE_BODY="Existing issue body with an old artifact oom-oldhash only."
bash -c "cd '${WORK}/c3' && '${SCRIPT}' migration_parser" >/dev/null
[ "$(count create)" = "0" ] && pass "create not called" || fail "create called (want 0)"
[ "$(count 'comment 42')" = "1" ] && pass "comment on #42 once" || fail "comment 42 count = $(count 'comment 42') (want 1)"
if grep -qF "${GITHUB_RUN_URL}" "${COMMENT_BODY}"; then pass "comment carries run URL"; else fail "comment missing run URL"; fi
EXPECTED_B64="$(printf 'crashing-input-bytes' | base64 --wrap=0)"
if grep -qF "${EXPECTED_B64}" "${COMMENT_BODY}"; then pass "comment carries base64 reproducer"; else fail "comment missing base64 reproducer"; fi

# --- Case 4: only a CLOSED same-title issue -> create (not a dedup target) -
echo "case 4: closed-only issue -> gh issue create"
setup_case "${WORK}/c4" "${ART}"
unset STUB_ISSUE_BODY
# `gh issue list --state open` finds nothing because the same-title issue is closed.
STUB_ISSUE_NUMBER="" bash -c "cd '${WORK}/c4' && '${SCRIPT}' migration_parser" >/dev/null
[ "$(count create)" = "1" ] && pass "create called once" || fail "create count = $(count create) (want 1)"
[ "$(grep -c '^comment ' "${CALL_LOG}" || true)" = "0" ] && pass "comment not called" || fail "comment called (want 0)"


EMPTY_ART="oom-da39a3ee5e6b4b0d3255bfef95601890afd80709" # SHA-1 of the empty string

# --- Case 5: all artifacts zero-byte -> no issue, no comment, exit 0 -------
echo "case 5: all-empty artifacts -> no create, no comment, cause named"
setup_case "${WORK}/c5" "${ART}"
rm -f "${WORK}/c5/fuzz/artifacts/migration_parser/${ART}"
: > "${WORK}/c5/fuzz/artifacts/migration_parser/${EMPTY_ART}"
unset STUB_ISSUE_BODY
OUT5="$(STUB_ISSUE_NUMBER="" bash -c "cd '${WORK}/c5' && '${SCRIPT}' migration_parser")"
[ "$(count create)" = "0" ] && pass "create not called" || fail "create called (want 0)"
[ "$(grep -c '^comment ' "${CALL_LOG}" || true)" = "0" ] && pass "comment not called" || fail "comment called (want 0)"
if grep -qF "zero-byte" <<<"${OUT5}"; then pass "output names the zero-byte cause"; else fail "output missing zero-byte cause"; fi

# --- Case 6: mixed empty + real artifact -> create from the real one only --
echo "case 6: mixed artifacts -> one create, empty artifact never a reproducer"
setup_case "${WORK}/c6" "${ART}"
: > "${WORK}/c6/fuzz/artifacts/migration_parser/${EMPTY_ART}"
unset STUB_ISSUE_BODY
STUB_ISSUE_NUMBER="" bash -c "cd '${WORK}/c6' && '${SCRIPT}' migration_parser" >/dev/null
[ "$(count create)" = "1" ] && pass "create called once" || fail "create count = $(count create) (want 1)"
if grep -qF "${ART}" "${CREATE_BODY}"; then pass "body carries the real artifact"; else fail "body missing real artifact"; fi
if grep -qF "${EMPTY_ART}" "${CREATE_BODY}"; then fail "body must not carry the empty artifact"; else pass "empty artifact absent from body"; fi
if grep -qF "zero-byte artifact(s) were skipped" "${CREATE_BODY}"; then pass "body notes the skip"; else fail "body missing skip note"; fi

echo
if [ "${FAILURES}" -eq 0 ]; then
  echo "All report-parser-fuzz-crashes dedup and zero-byte tests passed."
else
  echo "${FAILURES} assertion(s) failed."
  exit 1
fi
