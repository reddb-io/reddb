# Workflow cost policy

The automatic validation tier has two layers. `quick-checks.yml` checks changed
whitespace, workflow syntax, root/workspace manifests and this trigger policy on
one standard Ubuntu runner with a three-minute limit. `ci.yml` runs the core
correctness lane on code pull requests, merge groups and pushes to `main` or
`develop`; `ci-docs.yml` supplies the same required contexts for docs-only pull
requests without compiling the product.

Broader browser tests, coverage, benchmarks, cross-platform matrices and
operational deployments remain available through `workflow_dispatch`. The
expensive `full_ci` jobs inside `ci.yml` remain opt-in. Recurring validation and
watcher schedules are off.
Existing issue/comment routing remains available; releases only react to
intentional version tags, release events or an explicit manual action.
Artifact source/version/checksum and publication checks stay with publication.

## Standard public runners

CI, stable/RC builds and publication use standard GitHub-hosted runners:
`ubuntu-24.04`, `ubuntu-24.04-arm`, `windows-2025` and `macos-15`.
The ARM64 release leg remains native. Platform switches below retain their
existing defaults; this migration does not enable additional targets.

The paid `blacksmith-testbox.yml` is only an explicit hardware-comparison tool;
it requires a manually created Testbox session and is never called by CI or
release. The quick policy rejects Blacksmith labels in other workflows,
including labels embedded in JSON platform matrices.

Validate the actual stable release build path without publication:

```sh
gh workflow run release.yml --repo reddb-io/reddb --ref <branch> \
  -f channel=stable -f build_only=true -f publish_cargo=false
```

`build_only` defaults to the checked-out version and skips every publishing
job plus public-release asset verification. Binary builds, native smoke tests,
checksums and the artifact-size gate still execute. Normal tag/manual releases
retain their publication conditions. Rust compilation uses two concurrent jobs
on standard runners; the native runner capacity is recorded in job summaries.

## Run an optional workflow

In GitHub Actions, select its workflow, choose **Run workflow**, then select
the branch and inputs. From the CLI:

```sh
gh workflow run <file.yml> --repo reddb-io/reddb --ref main
```

A workflow using `workflow_call` is a component of its parent operation.
Run the parent rather than dispatching a component without its required inputs.
Manual deployment and publication commands retain their existing environment,
identity and version requirements; this policy does not publish anything itself.

## Available workflows

| File | Events |
| --- | --- |
| `blacksmith-testbox.yml` | workflow_dispatch |
| `browser-example.yml` | workflow_dispatch |
| `browser-release.yml` | push, workflow_dispatch |
| `browser.yml` | workflow_dispatch |
| `changesets.yml` | workflow_dispatch |
| `ci-docs.yml` | pull_request, workflow_dispatch |
| `ci.yml` | merge_group, push, pull_request, workflow_dispatch |
| `client-codecs.yml` | workflow_dispatch |
| `columnar-read-bench.yml` | workflow_dispatch |
| `crypto-coverage.yml` | workflow_dispatch |
| `dependabot.yml` | workflow_dispatch |
| `docs.yml` | workflow_dispatch |
| `drill-nightly.yml` | workflow_dispatch |
| `dst-nightly.yml` | workflow_dispatch |
| `file-coverage.yml` | workflow_dispatch |
| `grpc-runtime-checks.yml` | workflow_dispatch |
| `grpc-runtime-release.yml` | workflow_dispatch |
| `kv-bench.yml` | workflow_dispatch |
| `kv-compare-tests.yml` | workflow_dispatch |
| `operator-bootstrap-import.yml` | workflow_dispatch |
| `park-migration-regressions.yml` | workflow_dispatch |
| `parser-coverage.yml` | workflow_dispatch |
| `parser-fuzz-nightly.yml` | workflow_dispatch |
| `pgwire-clients.yml` | workflow_dispatch |
| `query-read-paths.yml` | workflow_dispatch |
| `red-issues-needs-triage.yml` | issues |
| `release-candidate.yml` | workflow_dispatch |
| `release.yml` | push, workflow_dispatch |
| `rql-coverage.yml` | workflow_dispatch |
| `rs-afk-attempt.yml` | workflow_dispatch |
| `runtime-qualification-artifact.yml` | workflow_dispatch |
| `security-audit.yml` | workflow_dispatch |
| `sql-reference.yml` | workflow_dispatch |
| `storage-apocalypse.yml` | workflow_dispatch |
| `tla-check.yml` | workflow_dispatch |
| `types-coverage.yml` | workflow_dispatch |
| `wire-coverage.yml` | workflow_dispatch |

Browser behavior/distribution checks run independently in `browser.yml`.
Compatibility runtime behavior checks run in `grpc-runtime-checks.yml`.

## Platform switches

Multiplatform workflows declare `ENABLE_*` variables in their top-level `env`.
Values must be `true` or `false`. Repository Actions variables with the same
names override the workflow defaults, so platforms can be changed without a commit.

| Variable | Default |
| --- | --- |
| `ENABLE_LINUX_X64` | `true` |
| `ENABLE_LINUX_ARM64` | `true` |
| `ENABLE_WINDOWS_X64` | `true` |
| `ENABLE_WINDOWS_ARM64` | `false` |
| `ENABLE_MACOS_X64` | `false` |
| `ENABLE_MACOS_ARM64` | `false` |
| `ENABLE_LINUX_ARMV7` | `false` |
| `ENABLE_LINUX_X86` | `false` |

`x64` means x86_64; `ARM64` means aarch64. glibc/musl and baseline variants
follow their OS/architecture switch. A flag only enables targets already supported
by that workflow; it does not add new build targets. Older tags keep their original
workflow configuration.

The lightweight `Select platforms` job filters matrices before platform runners
are allocated. Disabled platforms create no build/test runner. All native
platforms can be disabled; jobs that require their artifacts then skip. Portable
web packages and platform-independent orchestration keep their existing behavior. The
automatic Quick checks and core correctness lane remain separate from manual
release-adjacent jobs.

Example (change only future runs):

```sh
gh variable set ENABLE_MACOS_ARM64 --body true
gh variable set ENABLE_WINDOWS_ARM64 --body false
```

Inspect the `Select platforms` job summary to see the effective selection.

Select `preview_platforms: true` in Run workflow to execute only the lightweight
platform selector. Builds, tests and publication are all skipped in this mode.
