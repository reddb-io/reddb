#!/usr/bin/env bash
# Tests for scripts/report-parser-fuzz-crashes.sh.
#
# libFuzzer names artifacts by content hash, so a zero-byte artifact is named
# for the SHA-1 of the empty string (da39a3ee...) and carries no input to
# reproduce — it is the signature of an out-of-memory failure detected outside
# the execution of a unit (e.g. during corpus load). Filing a release-blocker
# issue from one produces an unreproducible report (issue #2093). These tests
# pin the fix: empty artifacts never become reproducers, an all-empty run files
# no issue, and the all-non-empty path stays byte-identical to the original.
#
# `gh` and `cargo` are stubbed so the tests never touch the live tracker and
# never invoke the real (slow, nightly-only) fuzzer.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="${SCRIPT_DIR}/report-parser-fuzz-crashes.sh"
TARGET="migration_parser"
EMPTY_HASH="da39a3ee5e6b4b0d3255bfef95601890afd80709" # SHA-1 of the empty string

WORKROOT="$(mktemp -d)"
trap 'rm -rf "${WORKROOT}"' EXIT

pass=0
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); echo "ok - $*"; }

# Build a per-case sandbox: a working directory (where fuzz/artifacts/<target>
# lives) plus a bin/ holding the gh and cargo stubs. Echoes the working dir.
make_case() {
  local name="$1"
  local case_dir="${WORKROOT}/${name}"
  local bin="${case_dir}/bin"
  mkdir -p "${case_dir}/fuzz/artifacts/${TARGET}" "${bin}" "${case_dir}/runner_temp"

  cat > "${bin}/gh" <<'STUB'
#!/usr/bin/env bash
# Records issue creation instead of contacting the live tracker.
if [ "${1:-}" = "issue" ] && [ "${2:-}" = "create" ]; then
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "--body-file" ]; then shift; cp "$1" "${GH_STUB_BODY}"; fi
    shift
  done
  : > "${GH_STUB_CREATED}"
  echo "https://github.com/reddb-io/reddb/issues/9999"
fi
exit 0
STUB

  cat > "${bin}/cargo" <<'STUB'
#!/usr/bin/env bash
# No-op: `cargo +nightly fuzz tmin` must not minimize under test, so the body
# reflects the original artifact bytes.
exit 0
STUB
  chmod +x "${bin}/gh" "${bin}/cargo"

  echo "${case_dir}"
}

# Run the script in a case sandbox. Sets GH_STUB_* so assertions can inspect
# whether an issue was created and with what body. Captures stdout to $2 and
# exit status to $3 (name refs).
run_script() {
  local case_dir="$1"
  local __out_var="$2"
  local __rc_var="$3"
  local out rc
  set +e
  out="$(cd "${case_dir}" && \
    PATH="${case_dir}/bin:${PATH}" \
    RUNNER_TEMP="${case_dir}/runner_temp" \
    GH_STUB_BODY="${case_dir}/issue-body.md" \
    GH_STUB_CREATED="${case_dir}/issue-created" \
    env -u GITHUB_RUN_URL bash "${SCRIPT}" "${TARGET}" 2>&1)"
  rc=$?
  set -e
  printf -v "${__out_var}" '%s' "${out}"
  printf -v "${__rc_var}" '%s' "${rc}"
}

# ---------------------------------------------------------------------------
# Case 1: only a zero-byte oom-* artifact -> no issue, exit 0, log names cause.
# ---------------------------------------------------------------------------
c1="$(make_case case1-all-empty)"
: > "${c1}/fuzz/artifacts/${TARGET}/oom-${EMPTY_HASH}"
run_script "${c1}" c1_out c1_rc

[ "${c1_rc}" = "0" ] || fail "case1: expected exit 0, got ${c1_rc} (out: ${c1_out})"
[ ! -e "${c1}/issue-created" ] || fail "case1: an issue was created for an all-empty run"
[ ! -e "${c1}/issue-body.md" ] || fail "case1: an issue body was produced for an all-empty run"
grep -qi "zero-byte" <<<"${c1_out}" || fail "case1: log did not mention zero-byte artifacts (out: ${c1_out})"
grep -qi "out-of-memory" <<<"${c1_out}" || fail "case1: log did not name the out-of-memory case (out: ${c1_out})"
grep -qi "corpus load" <<<"${c1_out}" || fail "case1: log did not name the likely cause corpus load (out: ${c1_out})"
ok "all-empty run files no issue, exits 0, and names the empty-artifact cause"

# ---------------------------------------------------------------------------
# Case 2: one zero-byte + one non-empty -> exactly one issue, non-empty only,
# body notes 1 empty artifact skipped.
# ---------------------------------------------------------------------------
c2="$(make_case case2-mixed)"
: > "${c2}/fuzz/artifacts/${TARGET}/oom-${EMPTY_HASH}"
printf 'boom' > "${c2}/fuzz/artifacts/${TARGET}/crash-real"
run_script "${c2}" c2_out c2_rc

[ "${c2_rc}" = "0" ] || fail "case2: expected exit 0, got ${c2_rc} (out: ${c2_out})"
[ -e "${c2}/issue-created" ] || fail "case2: no issue was created despite a non-empty artifact"
body2="$(cat "${c2}/issue-body.md")"
artifact_lines="$(grep -c '^Artifact: ' "${c2}/issue-body.md" || true)"
[ "${artifact_lines}" = "1" ] || fail "case2: expected exactly 1 Artifact section, got ${artifact_lines}"
grep -qF "Artifact: \`fuzz/artifacts/${TARGET}/crash-real\`" <<<"${body2}" \
  || fail "case2: body missing the non-empty reproducer"
grep -qF "oom-${EMPTY_HASH}" <<<"${body2}" \
  && fail "case2: body referenced the skipped empty artifact"
grep -qi "1 zero-byte artifact(s) were skipped" <<<"${body2}" \
  || fail "case2: body did not note that 1 empty artifact was skipped (body: ${body2})"
ok "mixed run files one issue from the non-empty artifact and notes 1 skipped"

# ---------------------------------------------------------------------------
# Case 3: only non-empty artifacts -> body byte-identical to the original
# script's output (pins existing behavior).
# ---------------------------------------------------------------------------
c3="$(make_case case3-nonempty)"
printf 'hello' > "${c3}/fuzz/artifacts/${TARGET}/crash-fixture"
run_script "${c3}" c3_out c3_rc

[ "${c3_rc}" = "0" ] || fail "case3: expected exit 0, got ${c3_rc} (out: ${c3_out})"
[ -e "${c3}/issue-created" ] || fail "case3: no issue was created"

expected="${c3}/expected-body.md"
cat > "${expected}" <<'GOLDEN'
Nightly parser fuzz found a failure in `migration_parser`.

- Workflow run: unknown
- Target: `migration_parser`
- Reproduce locally:

```bash
cargo +nightly install cargo-fuzz --locked
cargo +nightly fuzz run migration_parser fuzz/artifacts/migration_parser/<artifact> -- -rss_limit_mb=4096 -malloc_limit_mb=2048
```

## Minimized input

Artifact: `fuzz/artifacts/migration_parser/crash-fixture`

- Failure kind: crash

- Bytes: 5
- Base64:

```text
aGVsbG8=
```

GOLDEN

if ! diff -u "${expected}" "${c3}/issue-body.md"; then
  fail "case3: body is not byte-identical to the expected original output"
fi
grep -qi "skipped" <<<"$(cat "${c3}/issue-body.md")" \
  && fail "case3: all-non-empty body must not mention skipped artifacts"
ok "all-non-empty run produces a byte-identical body with no skip note"

echo "PASS - ${pass}/3 report-parser-fuzz-crashes cases"
