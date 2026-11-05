# Workflow cost policy

Requested by the maintainer on 2026-11-05: keep automatic CI small and make
expensive validation optional. `quick-checks.yml` is the only automatic
quality workflow. It checks changed whitespace, workflow syntax, root/workspace
manifests and this trigger policy on one standard Ubuntu runner, with a
three-minute limit. It does not compile products or run full test suites.

Integration/browser tests, coverage, benchmarks, cross-platform matrices and
operational deployments are available through `workflow_dispatch`. They are
not required PR statuses. Recurring validation and watcher schedules are off.
Existing issue/comment routing remains available; releases only react to
intentional version tags, release events or an explicit manual action.
Artifact source/version/checksum and publication checks stay with publication.

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
| `ci-docs.yml` | workflow_dispatch |
| `ci.yml` | workflow_dispatch |
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
