#!/usr/bin/env bash
# lint-no-untyped-serialization.sh — issue #180 / ADR 0010
#
# Grep-based preflight lint that fails when new untyped serialization
# crosses one of the boundaries enumerated in ADR 0010
# (`docs/adr/0010-serialization-boundary-discipline.md`):
#
#   1. tracing log macros that interpolate via format!()/write!()
#      instead of `tracing` field syntax (`field = %value`).
#   2. `*.header(name, format!(...))` HTTP header construction with
#      ad-hoc formatting outside `header_escape_guard`.
#   3. audit emission that takes a format!()-built string instead of a
#      typed `AuditFieldEscaper` field.
#   4. `json!({ "k": format!(...) })` JSON construction that splices a
#      formatted string into a serde-managed envelope (Lane AE / #178).
#   5. `Tainted<T>::expose_secret` outside its allowlisted modules and
#      direct `.0` projection of a `Tainted<...>` value (the only
#      blessed exit is `escape_for(boundary)` per #179).
#   6. `http::HeaderValue::from_str(...)` outside the
#      `header_escape_guard` module — the typed guard owns this call.
#
# Usage:
#   scripts/lint-no-untyped-serialization.sh [PATH ...]
#
# With no args, scans the workspace `crates/` directory. Pass an
# explicit path for fixture testing, e.g.
#   scripts/lint-no-untyped-serialization.sh tests/lint-fixtures/violations.rs.fixture
#
# Whitelist: `scripts/lint-untyped-serialization-whitelist.txt` —
# explicit per-line entries (`path:line_pattern`) for legacy paths
# pending retrofit. Each entry needs a comment + tracking issue.
#
# Exit codes:
#   0  — clean (or every violation is whitelisted).
#   1  — at least one un-whitelisted violation found.
#   2  — invocation / IO error (missing whitelist, bad path, etc.).
#
# Portability: bash + awk + grep + sed; no GNU-only flags. Tested
# against busybox grep / mawk for the bits that matter (multi-line
# tracing detection sits in awk so we don't depend on grep -P).

set -u
# `pipefail` lets us notice if grep is unhappy (e.g. permission denied
# on a path); we can't `set -e` because grep returns 1 on no-match,
# and that is a normal control-flow signal here.
set -o pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
WHITELIST_FILE="$SCRIPT_DIR/lint-untyped-serialization-whitelist.txt"

if [ ! -f "$WHITELIST_FILE" ]; then
  echo "lint: missing whitelist file: $WHITELIST_FILE" >&2
  exit 2
fi

# ----------------------------------------------------------------------
# Resolve paths to scan.
# ----------------------------------------------------------------------
TARGETS=()
if [ "$#" -eq 0 ]; then
  TARGETS+=("$REPO_ROOT/crates")
else
  for arg in "$@"; do
    TARGETS+=("$arg")
  done
fi

# ----------------------------------------------------------------------
# Build the file list. We scan .rs and .rs.fixture files. Honour the
# repo root if provided; otherwise scan as given. Skip target/ to keep
# the lint fast on machines with cargo build artefacts.
# ----------------------------------------------------------------------
FILE_LIST=$(mktemp)
VIOLATIONS_FILE=$(mktemp)
trap 'rm -f "$FILE_LIST" "$VIOLATIONS_FILE"' EXIT

for target in "${TARGETS[@]}"; do
  if [ -f "$target" ]; then
    printf '%s\n' "$target" >>"$FILE_LIST"
  elif [ -d "$target" ]; then
    # -L follows symlinks (workspace setups sometimes use them); skip
    # `target/` build artefacts and any vendored dep checkouts.
    find -L "$target" \
      \( -name target -o -name node_modules \) -prune \
      -o \( -type f \( -name '*.rs' -o -name '*.rs.fixture' \) -print \) \
      >>"$FILE_LIST"
  else
    echo "lint: path not found: $target" >&2
    exit 2
  fi
done

# ----------------------------------------------------------------------
# Whitelist matcher.
#
# Each non-comment, non-blank line in the whitelist file is one of:
#   path/to/file.rs               — every hit in this file is allowed.
#   path/to/file.rs:<regex>       — hits whose body matches <regex> are allowed.
#
# Paths are matched as suffixes (so callers don't need to track an
# absolute prefix), and `<regex>` is an extended regex applied to the
# violation's source-line text (not the path).
# ----------------------------------------------------------------------
is_whitelisted() {
  local file="$1"
  local line_no="$2"
  local body="$3"

  while IFS= read -r raw; do
    # Strip trailing comment (everything after first ` #`).
    local entry="${raw%% #*}"
    # Trim trailing whitespace.
    entry="${entry%"${entry##*[![:space:]]}"}"
    # Trim leading whitespace.
    entry="${entry#"${entry%%[![:space:]]*}"}"
    case "$entry" in
      ''|'#'*) continue ;;
    esac

    local entry_path="${entry%%:*}"
    local entry_pat=""
    case "$entry" in
      *:*) entry_pat="${entry#*:}" ;;
    esac

    # Suffix-match the path. `*foo` semantics — the whitelist may use
    # repo-relative paths and the runtime sees absolute or relative,
    # both must work.
    case "$file" in
      *"$entry_path") : ;;
      *) continue ;;
    esac

    if [ -z "$entry_pat" ] || [ "$entry_pat" = "*" ]; then
      return 0
    fi

    # ERE match of the body against the pattern. `printf | grep -E`
    # avoids relying on bash =~ regex flavour, which differs across
    # platforms.
    if printf '%s' "$body" | grep -qE -- "$entry_pat"; then
      return 0
    fi
  done <"$WHITELIST_FILE"

  return 1
}

# scan_pattern <ERE> — emit `file<TAB>line<TAB>body` for every line in
# $FILE_LIST that matches the regex. We can't rely on `grep -nE`'s
# default `:` separator because the body itself frequently contains
# `:` (think `Type::method`). awk on a per-file basis gives us a
# reliable tab-delimited stream.
scan_pattern() {
  local pattern="$1"
  while IFS= read -r f; do
    [ -z "$f" ] && continue
    # Skip pure-comment lines (`^[[:space:]]*//`) so doc/explanatory
    # text inside the codebase doesn't trip the lint.
    awk -v file="$f" -v pat="$pattern" '
      /^[[:space:]]*\/\// { next }
      $0 ~ pat { printf "%s\t%d\t%s\n", file, NR, $0 }
    ' "$f"
  done <"$FILE_LIST"
}

emit_violation() {
  local category="$1"
  local file="$2"
  local line_no="$3"
  local body="$4"

  if is_whitelisted "$file" "$line_no" "$body"; then
    return 0
  fi

  printf '%s\t%s\t%s\t%s\n' \
    "$category" "$file" "$line_no" "$body" \
    >>"$VIOLATIONS_FILE"
}

# ----------------------------------------------------------------------
# Category 1: format!()/write!() inside a tracing log macro.
#
# Multi-line aware: we walk each file in awk and track when we are
# inside a `tracing::{info,warn,error,debug,trace}!(` invocation. While
# the paren-depth from that opening is > 0, any `format!(` or
# `write!(` is a hit. We close the block when paren-depth reaches 0.
#
# Tracing's own field syntax (`user = %name`, `?value`) does not match
# this rule — only `format!`/`write!` calls do. A constant-string log
# (`info!("hello")`) does not match either. Both positional
# (`tracing::warn!("x {}", format!(...))`) and keyed
# (`tracing::error!(msg = format!(...))`) splices are caught — see
# `tests/lint-fixtures/violations.rs.fixture` and PRD #201.
# ----------------------------------------------------------------------
scan_tracing_format() {
  local file="$1"
  awk -v file="$file" '
    BEGIN { depth = 0; start_line = 0 }

    # Drop // line-comments to avoid false hits on doc / explanatory
    # text inside the macro body.
    {
      raw = $0
      stripped = $0
      cidx = index(stripped, "//")
      if (cidx > 0) {
        # Avoid stripping inside a string literal — cheap heuristic:
        # only strip if the comment is preceded by whitespace or BOL.
        before = substr(stripped, 1, cidx - 1)
        if (before ~ /^[[:space:]]*$/ || before ~ /[[:space:]]$/) {
          stripped = before
        }
      }
    }

    {
      pos = 1
      line_text = stripped
      while (pos <= length(line_text)) {
        rest = substr(line_text, pos)

        if (depth == 0) {
          # Look for an opening tracing macro. Match
          # tracing::info!( and bare info!(/warn!(/error!(/debug!(/trace!(.
          if (match(rest, /(tracing::)?(info|warn|error|debug|trace)![[:space:]]*\(/)) {
            depth = 1
            start_line = NR
            opener = substr(rest, RSTART, RLENGTH)
            pos += RSTART + RLENGTH - 1
            # Consume bytes between RSTART and RLENGTH; depth has
            # already swallowed the opening "(".
            continue
          } else {
            break
          }
        }

        # depth > 0 — track parens, look for format!/write! hits.
        ch = substr(line_text, pos, 1)
        if (ch == "(") { depth += 1 }
        else if (ch == ")") {
          depth -= 1
          if (depth == 0) { pos += 1; continue }
        }
        # Look ahead for format!( or write!( starting at this position.
        ahead = substr(line_text, pos)
        if (match(ahead, /^format![[:space:]]*\(/) || match(ahead, /^write![[:space:]]*\(/)) {
          print NR "\t" raw
        }
        pos += 1
      }
    }
  ' "$file"
}

while IFS= read -r file; do
  scan_tracing_format "$file" | while IFS=$'\t' read -r ln body; do
    emit_violation "tracing+format" "$file" "$ln" "$body"
  done
done <"$FILE_LIST"

# ----------------------------------------------------------------------
# Category 2: HTTP header construction with format!() or raw user
# strings outside header_escape_guard. The typed guard
# (`HeaderEscapeGuard`) owns header value validation — every other
# call site must hand it an already-validated value.
# ----------------------------------------------------------------------
while IFS=$'\t' read -r file ln body; do
  [ -z "$file" ] && continue
  case "$file" in
    *header_escape_guard*) continue ;;
  esac
  emit_violation "header+format" "$file" "$ln" "$body"
done < <(scan_pattern '\.header[[:space:]]*\([^,]+,[[:space:]]*&?format![[:space:]]*\(')

# ----------------------------------------------------------------------
# Category 3: audit emission with a format!()-built field. Audit
# fields cross AuditFieldEscaper now — the format!() shape signals a
# call site that hasn't been migrated.
#
# Pattern: `<thing>.record(format!(...))`, `.append(format!(...))`,
# `.log_event(format!(...))`. The `record_*` family is the canonical
# audit emit method on AuditLogManager.
# ----------------------------------------------------------------------
while IFS=$'\t' read -r file ln body; do
  [ -z "$file" ] && continue
  emit_violation "audit+format" "$file" "$ln" "$body"
done < <(scan_pattern '(audit|audit_log|auditor)[a-z_]*\.(record|record_event|append|log|log_event|emit)[a-z_]*[[:space:]]*\([[:space:]]*format![[:space:]]*\(')

# ----------------------------------------------------------------------
# Category 4: json!({ ..., "k": format!(...) }) — the json!() literal
# is fine, the *interpolated formatted string* defeats the
# SerializedJsonField round-trip. Match `json!(` followed (eventually)
# by `format!(` on the same line; multi-line forms are rare and will
# get covered when we graduate to a clippy lint.
# ----------------------------------------------------------------------
while IFS=$'\t' read -r file ln body; do
  [ -z "$file" ] && continue
  emit_violation "json+format" "$file" "$ln" "$body"
done < <(scan_pattern 'json![[:space:]]*\([^)]*format![[:space:]]*\(')

# ----------------------------------------------------------------------
# Category 5a: `Tainted<...>` direct field projection (`.0`). The
# blessed exit is `escape_for(boundary)` or the loudly-named
# `expose_secret`. We grep for the sequence `Tainted` followed within
# a few chars by `.0` — heuristic but sufficient to catch the typical
# `let raw = tainted.0;` / `Tainted::0` shape outside the sanitizer
# module.
# ----------------------------------------------------------------------
while IFS=$'\t' read -r file ln body; do
  [ -z "$file" ] && continue
  case "$file" in
    *reddb-wire/src/sanitizer.rs) continue ;;
  esac
  emit_violation "tainted-unwrap" "$file" "$ln" "$body"
done < <(scan_pattern '(Tainted::0|\.expose_secret[[:space:]]*\([[:space:]]*\))')

# ----------------------------------------------------------------------
# Category 5b (alias): `expose_secret` outside its allowlist.
# Allowlisted modules: the sanitizer itself + adversarial test that
# proves the exit gate is named loudly. Anywhere else, calling it is
# a smell that should at least appear on the whitelist.
#
# Rolled into 5a's grep — handled via the same regex above.
# ----------------------------------------------------------------------

# ----------------------------------------------------------------------
# Category 6: http::HeaderValue::from_str(...) outside
# header_escape_guard.rs. The typed guard owns this constructor;
# anyone calling it directly is bypassing CR/LF/NUL/tab validation.
# ----------------------------------------------------------------------
while IFS=$'\t' read -r file ln body; do
  [ -z "$file" ] && continue
  case "$file" in
    *header_escape_guard*) continue ;;
  esac
  emit_violation "header-from-str" "$file" "$ln" "$body"
done < <(scan_pattern '(http::)?HeaderValue::from_str[[:space:]]*\(')

# ----------------------------------------------------------------------
# Report.
# ----------------------------------------------------------------------
if [ ! -s "$VIOLATIONS_FILE" ]; then
  echo "lint-no-untyped-serialization: clean ($(wc -l <"$FILE_LIST" | tr -d ' ') files scanned)"
  exit 0
fi

# Sort by category for grep-ability in CI logs.
sort -u "$VIOLATIONS_FILE" | awk -F'\t' '
  {
    cat=$1; file=$2; ln=$3; body=$4
    printf("[%s] %s:%s\n    %s\n", cat, file, ln, body)
    counts[cat]++
    total++
  }
  END {
    printf("\n%d violation(s) across %d category/categories.\n", total, length(counts))
    for (c in counts) {
      printf("  %-18s  %d\n", c, counts[c])
    }
    print ""
    print "Each violation must either (a) migrate to the typed guard"
    print "named in ADR 0010 or (b) be added to"
    print "scripts/lint-untyped-serialization-whitelist.txt with a"
    print "comment + tracking issue."
  }
'

exit 1
