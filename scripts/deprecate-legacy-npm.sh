#!/usr/bin/env bash
# Deprecate the legacy `reddb-cli` npm package — pre-@reddb-io migration name.
#
# Run ONCE post-1.0.0 publish, by an operator with publish rights to
# `reddb-cli` on the npm registry. NOT wired into release.yml: a CI-driven
# deprecate-on-every-release loop is too easy to misfire.
#
# Pre-reqs:
#   - `npm whoami` returns a maintainer of `reddb-cli`
#   - `@reddb-io/cli` 1.0.0 has already been published (so the redirect target exists)
#
# Effect:
#   - Marks every published version of `reddb-cli` as deprecated with a
#     pointer to `@reddb-io/cli`. `npm install reddb-cli` still works, but
#     the user sees a deprecation warning at install time.
#   - Idempotent: re-running just rewrites the same deprecation message.
#
# To undeprecate: `npm deprecate reddb-cli@"<all-versions>" ""`

set -euo pipefail

npm deprecate reddb-cli@"<all-versions>" "Renamed to @reddb-io/cli — please install that instead. See https://github.com/reddb-io/reddb#install"

echo "deprecated reddb-cli on npm — users now redirected to @reddb-io/cli"
