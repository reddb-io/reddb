#!/usr/bin/env bash
set -euo pipefail

target="${1:?usage: report-parser-fuzz-crashes.sh <target>}"
artifact_dir="fuzz/artifacts/${target}"
fuzz_rss_limit_mb="${FUZZ_RSS_LIMIT_MB:-4096}"
fuzz_malloc_limit_mb="${FUZZ_MALLOC_LIMIT_MB:-2048}"

if [ ! -d "${artifact_dir}" ] || ! find "${artifact_dir}" -type f | grep -q .; then
  echo "No fuzz artifacts found for ${target}; skipping issue creation."
  exit 0
fi

# Partition artifacts into real reproducers (non-empty) and zero-byte files.
# libFuzzer names artifacts by content hash, so a zero-byte artifact is named
# for the SHA-1 of the empty string (da39a3ee...) and carries no input to
# reproduce. That happens when an OOM is detected outside the execution of a
# specific unit — for example while the evolved corpus cache is being loaded —
# so the empty file is a signal about the run, not about an input. Filing an
# issue from it produces an unreproducible report, so we exclude empty
# artifacts here and never let one become a reproducer.
empty_count=0
non_empty_artifacts=()
while IFS= read -r artifact; do
  if [ -s "${artifact}" ]; then
    non_empty_artifacts+=("${artifact}")
  else
    empty_count=$((empty_count + 1))
    echo "Skipping zero-byte artifact ${artifact} (no input to reproduce)."
  fi
done < <(find "${artifact_dir}" -type f | sort)

if [ "${#non_empty_artifacts[@]}" -eq 0 ]; then
  echo "All ${empty_count} fuzz artifact(s) for ${target} are zero-byte; not filing an issue."
  echo "Empty artifacts are the signature of an out-of-memory failure detected outside the execution of a unit (for example during corpus load), which leaves no reproducer input. Inspect the fuzzer log in the workflow run to diagnose the run."
  exit 0
fi

gh label create "release-blocker" \
  --color "B60205" \
  --description "Blocks release until resolved" >/dev/null 2>&1 || true

header="$(mktemp)"
tmin_log="$(mktemp)"
b64_file="$(mktemp)"
create_body="$(mktemp)"
comment_body="$(mktemp)"
sections_dir="$(mktemp -d)"
title_suffix="crash"
trap 'rm -rf "${header}" "${tmin_log}" "${b64_file}" "${create_body}" "${comment_body}" "${sections_dir}"' EXIT

{
  echo "Nightly parser fuzz found a failure in \`${target}\`."
  echo
  echo "- Workflow run: ${GITHUB_RUN_URL:-unknown}"
  echo "- Target: \`${target}\`"
  echo "- Reproduce locally:"
  echo
  echo '```bash'
  echo "cargo +nightly install cargo-fuzz --locked"
  echo "cargo +nightly fuzz run ${target} fuzz/artifacts/${target}/<artifact> -- -rss_limit_mb=${fuzz_rss_limit_mb} -malloc_limit_mb=${fuzz_malloc_limit_mb}"
  echo '```'
  echo
  echo "## Minimized input"
  echo
} > "${header}"

if [ "${empty_count}" -gt 0 ]; then
  {
    echo "> Note: ${empty_count} zero-byte artifact(s) were skipped (out-of-memory outside a unit, e.g. during corpus load; no reproducer input)."
    echo
  } >> "${header}"
fi

# Build one Markdown section per artifact, keyed by the libFuzzer artifact
# basename. The basename is a content hash, so it is a stable dedup key: an
# identical reproducer found on a later night has the same basename and adds
# nothing, while a genuinely new input carries a new basename.
declare -a basenames=()
idx=0
for artifact in "${non_empty_artifacts[@]}"; do
  base="$(basename "${artifact}")"
  minimized="${RUNNER_TEMP:-/tmp}/${target}-${base}.min"
  cp "${artifact}" "${minimized}"
  artifact_kind="crash"
  if [[ "${base}" == oom-* ]]; then
    artifact_kind="out-of-memory"
    title_suffix="out-of-memory"
  fi

  # Best effort: cargo-fuzz/libFuzzer usually leaves a small reproducer already,
  # but try tmin so the issue body carries the smallest input this runner can find.
  cargo +nightly fuzz tmin "${target}" "${minimized}" -- -max_total_time=60 >"${tmin_log}" 2>&1 || true

  {
    echo "Artifact: \`${artifact}\`"
    echo
    echo "- Failure kind: ${artifact_kind}"
    echo
    echo "- Bytes: $(wc -c < "${minimized}")"
    echo "- Base64:"
    echo
    echo '```text'
    if base64 --wrap=0 "${minimized}" >"${b64_file}" 2>/dev/null; then
      cat "${b64_file}"
    else
      base64 "${minimized}"
    fi
    echo
    echo '```'
    echo
  } > "${sections_dir}/${idx}"
  basenames+=("${base}")
  idx=$((idx + 1))
done

title="release-blocker: parser fuzz ${title_suffix} in ${target}"

# One open issue per (target, failure-kind): the title is fully determined by
# ${title_suffix} and ${target}, so it doubles as the dedup key. Only OPEN
# issues are dedup targets — a closed-but-recurring failure means the fix that
# closed it regressed, and that occurrence deserves its own fresh history.
issue_number="$(gh issue list --state open --label release-blocker \
  --search "in:title \"parser fuzz ${title_suffix} in ${target}\"" \
  --json number,title \
  --jq "map(select(.title == \"${title}\")) | .[0].number // empty")"

if [ -z "${issue_number}" ]; then
  # No open match: file a fresh issue carrying every reproducer from this run.
  cat "${header}" > "${create_body}"
  for i in "${!basenames[@]}"; do
    cat "${sections_dir}/${i}" >> "${create_body}"
  done
  gh issue create \
    --title "${title}" \
    --label "release-blocker" \
    --body-file "${create_body}"
  exit 0
fi

# Open match exists: comment only with reproducers whose basename is not already
# recorded in the issue body or any of its comments. Re-found identical inputs
# add nothing.
existing_content="$(gh issue view "${issue_number}" --json body,comments \
  --jq '.body, (.comments[]?.body)')"

declare -a new_indices=()
for i in "${!basenames[@]}"; do
  if ! grep -qF -- "${basenames[$i]}" <<<"${existing_content}"; then
    new_indices+=("${i}")
  fi
done

if [ "${#new_indices[@]}" -eq 0 ]; then
  # Known failure recurred with no new inputs: stay silent, just log.
  echo "Known failure \"${title}\" recurred in ${GITHUB_RUN_URL:-unknown} with no new inputs (#${issue_number}); nothing to report."
  exit 0
fi

{
  echo "Nightly parser fuzz reproduced this failure again."
  echo
  echo "- Workflow run: ${GITHUB_RUN_URL:-unknown}"
  echo
  echo "## New reproducers"
  echo
} > "${comment_body}"
for i in "${new_indices[@]}"; do
  cat "${sections_dir}/${i}" >> "${comment_body}"
done

gh issue comment "${issue_number}" --body-file "${comment_body}"
