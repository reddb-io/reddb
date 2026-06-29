#!/usr/bin/env bash
set -euo pipefail

target="${1:?usage: report-parser-fuzz-crashes.sh <target>}"
artifact_dir="fuzz/artifacts/${target}"

if [ ! -d "${artifact_dir}" ] || ! find "${artifact_dir}" -type f | grep -q .; then
  echo "No fuzz artifacts found for ${target}; skipping issue creation."
  exit 0
fi

gh label create "release-blocker" \
  --color "B60205" \
  --description "Blocks release until resolved" >/dev/null 2>&1 || true

body="$(mktemp)"
tmin_log="$(mktemp)"
b64_file="$(mktemp)"
title_suffix="crash"
trap 'rm -f "${body}" "${tmin_log}" "${b64_file}"' EXIT
{
  echo "Nightly parser fuzz found a failure in \`${target}\`."
  echo
  echo "- Workflow run: ${GITHUB_RUN_URL:-unknown}"
  echo "- Target: \`${target}\`"
  echo "- Reproduce locally:"
  echo
  echo '```bash'
  echo "cargo +nightly install cargo-fuzz --locked"
  echo "cargo +nightly fuzz run ${target} fuzz/artifacts/${target}/<artifact>"
  echo '```'
  echo
  echo "## Minimized input"
  echo
} > "${body}"

while IFS= read -r artifact; do
  minimized="${RUNNER_TEMP:-/tmp}/${target}-$(basename "${artifact}").min"
  cp "${artifact}" "${minimized}"
  artifact_kind="crash"
  if [[ "$(basename "${artifact}")" == oom-* ]]; then
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
  } >> "${body}"
done < <(find "${artifact_dir}" -type f | sort)

gh issue create \
  --title "release-blocker: parser fuzz ${title_suffix} in ${target}" \
  --label "release-blocker" \
  --body-file "${body}"
