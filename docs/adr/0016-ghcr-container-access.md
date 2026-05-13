# ADR 0016 — GHCR Container Access Model

**Status:** Accepted

## Context

RedDB publishes container images under `ghcr.io/reddb-io/reddb` and
`ghcr.io/reddb-io/reddb-client`. At the time of this decision, the GHCR
packages may require GitHub Container Registry authentication. Anonymous
`docker pull ghcr.io/reddb-io/reddb:latest` can fail with `unauthorized`
depending on package visibility.

The in-repo Compose examples build from the local checkout and do not require
GHCR access. Documentation snippets that use prebuilt `ghcr.io` images must
describe the registry access requirement explicitly.

## Decision

Use option **(b): document authenticated GHCR access** until the package
visibility is deliberately flipped public by a repository/package admin.

Users who pull prebuilt GHCR images should run:

```bash
echo "$GITHUB_TOKEN" | docker login ghcr.io -u "$GITHUB_USER" --password-stdin
docker pull ghcr.io/reddb-io/reddb:latest
```

For anonymous/local quickstarts, prefer either:

- install/run the `red` binary directly, or
- build the image from the checkout with `docker build -t reddb .`, or
- use the in-repo Compose examples, which use `build:` rather than a GHCR
  `image:`.

## Follow-up

If `ghcr.io/reddb-io/reddb` is made public later, update this ADR and remove
the `docker login ghcr.io` prerequisite from user-facing docs.
